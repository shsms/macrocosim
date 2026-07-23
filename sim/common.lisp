;; switchyard runtime helpers — load this from your config file before
;; anything else:
;;
;;   (unless (boundp 'switchyard-loaded)
;;     (setq switchyard-loaded t)
;;     (load "sim/common.lisp"))
;;
;; Built on tulisp-async's `run-with-timer` / `cancel-timer`. Lisp's
;; job is to wire the topology and animate the *environment* (per-tick
;; voltage / frequency perturbations, scheduled events). Component
;; physics — ramps, SoC derating, AC/DC conversion — lives in Rust and
;; is reached via the `make-*` constructors and the gRPC API.

;; -----------------------------------------------------------------------------
;; Timer bookkeeping
;; -----------------------------------------------------------------------------

;; Live timer handles created by `every` and friends. Tracked so that
;; reset-state can cancel them on a config reload — otherwise the old
;; callbacks keep firing into a fresh, unrelated microgrid.
;;
;; Process-global by design. If two distinct scenario scripts ever
;; share a process (rare with the current single-Config design),
;; reset-state would cancel both. Switchyard runs one Config per
;; process, so today there's no namespace conflict; revisit if a
;; multi-config layout shows up.
(unless (boundp 'active-timers)
  (setq active-timers nil))

(defun cancel-timers ()
  "Cancel every timer tracked on `active-timers` (pushed by `every`,
`scenario-end-after`, and the random-outage chain). Process-wide.
Reload calls this centrally before replaying the loaded files, so
scripts do NOT call it themselves — a script's own call, replayed
after another script, would cancel that script's freshly
re-registered timers. Call it manually only before re-(load)ing a
script into a live world."
  (dolist (tm active-timers)
    (cancel-timer tm))
  (setq active-timers nil))

(defun reset-state ()
  "`cancel-timers`, then wipe the active microgrid's components.
For scripts that own their whole world; a script loaded into a
live multi-microgrid engine usually wants plain `cancel-timers`,
which leaves the ambient microgrid's site alone."
  (cancel-timers)
  (reset-microgrid))

;; -----------------------------------------------------------------------------
;; Periodic helper
;; -----------------------------------------------------------------------------

(defun every (&rest plist)
  "Call :call every :milliseconds ms. First firing happens after the
interval has elapsed — not synchronously at load time — so a config
file can put `every` blocks anywhere relative to the topology they
reference.

Optional :args is a list passed as positional arguments to the
callback on every firing — `(every :call 'fire :args (list 1001))`
calls `(fire 1001)` each tick, saving a closing lambda. Defaults
to no extra args.

The handle is pushed onto `active-timers` so reset-state can
cancel it on reload."
  (let* ((ms (plist-get plist :milliseconds))
         (func (plist-get plist :call))
         (args (plist-get plist :args))
         (secs (/ ms 1000.0)))
    (setq active-timers
          (cons (apply 'run-with-timer secs secs func args)
                active-timers))))

;; -----------------------------------------------------------------------------
;; In-sim controller
;; -----------------------------------------------------------------------------

(defun define-controller (&rest plist)
  "Register an in-sim controller: call the :on-tick lambda every
:every-ms ms (default 100) on Config's refresh loop. A controller is the
closed-loop counterpart to the open-loop drivers — it *senses* live
state and *actuates* in response, modelling an EMS / dispatcher.

Inside :on-tick, read live state with `component-active-power`,
`component-bound-lower`, and `component-bound-upper` (the latter two
report the effective bounds, so they follow any cap a bounds-driving app
has applied), and actuate with `set-active-power` —
pass its CLAMP arg `t` to command \"as much as the live cap allows\"
without tracking the augmentations yourself. eg. a dispatcher that keeps
a battery inverter charging at the highest power the live cap allows:

  (define-controller :id 'ems
    :on-tick (lambda () (set-active-power 300 (component-bound-upper 300) 2000 t)))

The optional :id is a readability/label. The timer is tracked on
`active-timers` so `reset-state` cancels it on reload.

Cadence vs command-delay: each command takes :command-delay-ms to
execute, and while one executes only the newest incoming command waits
for its turn. A controller that re-sends every :every-ms therefore
trails the target by about one delay; re-sending faster than the delay
is safe (it never starves the device), it just wastes commands."
  (let* ((on-tick (plist-get plist :on-tick))
         (req-ms (plist-get plist :every-ms))
         (ms (if req-ms req-ms 100)))
    (every :milliseconds ms :call on-tick)))

;; -----------------------------------------------------------------------------
;; UI override file loader
;; -----------------------------------------------------------------------------

(defun overrides-path ()
  "Path of the per-microgrid UI overrides file, relative to the
config's load directory. Mirrors where every successful /api/eval
form is appended. Reads `(current-microgrid-id)`, which inside a
make-microgrid `:topology` lambda resolves to the entry being built.
The file sits next to the per-mg config under microgrids/."
  (format "microgrids/config.%d.overrides.lisp" (current-microgrid-id)))

(defun load-overrides ()
  "Load the persisted UI overrides for this microgrid if they exist.
No-op on a fresh checkout. Call from inside a make-microgrid
:topology lambda so the load happens with the per-mg current-microgrid
context active."
  (let ((path (overrides-path)))
    (when (file-exists-p path)
      (load path))))

;; -----------------------------------------------------------------------------
;; Scenario helpers
;; -----------------------------------------------------------------------------

(defun scenario-end-after (minutes)
  "Schedule a single-shot timer that runs (scenario-stop) after
MINUTES wall-clock minutes (not seconds — most other DSL ops are
seconds or milliseconds; this one is minutes because the use case
is fixed-duration runs sized in minutes, e.g.
`(scenario-end-after 60)` for a one-hour cap). The handle goes
through `every`'s tracker so a reload cancels it."
  (let ((secs (* minutes 60.0)))
    (setq active-timers
          (cons (run-with-timer secs nil 'scenario-stop)
                active-timers))))
