;; Berlin demo — a self-contained switchyard world: one microgrid
;; (id 2200) with battery, solar, EV-charger, CHP and consumer
;; branches, its environment animation, and the seven starter
;; scenarios that drive it.
;;
;; Run it as the boot script:
;;
;;   cargo run --bin switchyard examples/berlin-demo.lisp
;;
;; or load it into a bare engine (`cargo run --bin switchyard`) at
;; runtime, from the REPL box or the Microgrids tab:
;;
;;   (load "examples/berlin-demo.lisp")
;;
;; A relative path resolves against the state dir (--state-dir,
;; default: the directory the server was started from).
;;
;; Component ids are pinned throughout. Auto ids come from an
;; enterprise-wide allocator, so they depend on what else loaded
;; first — and the scenarios below address components by id.

;; No timer hygiene needed here: a hot-reload cancels every timer
;; centrally before replaying the loaded files, so each script's
;; `every` blocks re-register exactly once. (Only re-(load)ing a
;; script into a LIVE world stacks its timers — run (cancel-timers)
;; first in that case, or just save a watched file to reload.)

;; -----------------------------------------------------------------------------
;; Enterprise-level identity + grid frequency
;; -----------------------------------------------------------------------------

(set-enterprise-id 1)

;; Grid frequency — one Ornstein-Uhlenbeck process per process,
;; shared by every microgrid in the registry (frequency is a
;; property of the AC grid, not the microgrid). Values below pick
;; a healthy synchronous-grid shape: ~47 mHz equilibrium std dev
;; (σ / sqrt(2k) with σ = 0.015 Hz/√s and k = 0.05 /s), ~20-second
;; correlation time. Scenarios pull toward a specific value via
;; `(override-frequency-model :nominal F)` / `(clear-frequency-override)`.
(set-frequency-model
 :nominal       50.0
 :mean-rev-rate  0.05
 :sigma          0.015)

;; -----------------------------------------------------------------------------
;; Environment animation
;; -----------------------------------------------------------------------------

;; Per-tick noise on the AC line voltage — a slow random wander a
;; few hundred mV either side of nominal. Applies to the active
;; microgrid; the scenarios per-microgrid replay fans it out.
(every
 :milliseconds 200
 :call (lambda ()
         (set-voltage-per-phase
          (+ 229.0 (/ (random 200) 100.0))
          (+ 229.0 (/ (random 200) 100.0))
          (+ 229.0 (/ (random 200) 100.0)))))

;; PV cloud-cover schedule over a 10-minute window, driving the solar
;; inverter (id 200). Sunny first 3 min (80%), 2-min ramp into clouds
;; (→ 20%), 2 min cloudy, 2-min ramp back to clear. Installed as the
;; inverter's :sunlight% lambda SOURCE below rather than an
;; imperative timer: a timer would overwrite a scenario's numeric
;; sunlight set within a second, while a scenario's numeric set
;; cleanly collapses a source and takes over.
(defun cloud-curve (t-window)
  (cond ((< t-window 180.0) 80.0)
        ((< t-window 300.0) (- 80.0 (* 0.5 (- t-window 180.0))))
        ((< t-window 420.0) 20.0)
        (t (min 80.0 (+ 20.0 (* 0.5 (- t-window 420.0)))))))

;; -----------------------------------------------------------------------------
;; Topology
;; -----------------------------------------------------------------------------

(make-microgrid
 :id 2200
 :name "Berlin demo"
 :grpc-port 8800
 :tso "TN"
 :topology
 (lambda ()
   (make-grid-connection-point
    :id 1
    :rated-lower -90000.0
    :rated-upper  100000.0
    :successors
    (list
     (make-meter
      :id 2                          ;; grid's sole child → derived as the main/PCC meter
      :successors
      (list
       ;; Battery branch — every knob (SCADA delay, ramp, jitter,
       ;; kVA-circle reactive envelope) comes from battery-inverter-
       ;; defaults / battery-defaults.
       (make-meter
        :id 1002
        :successors
        (list (make-battery-inverter
               :id 1001
               :successors
               (list (make-battery :id 1000 :initial-soc 85.0)))))

       ;; Solar branch — sunlight driven by the cloud curve as a
       ;; lambda source (see the comment on cloud-curve above).
       (make-meter
        :id 1003
        :successors
        (list (make-solar-inverter
               :id 200
               :sunlight% (lambda () (cloud-curve (window-elapsed 600.0))))))

       ;; EV branch — near-full so the SoC-protect taper is observable.
       (make-meter
        :id 1005
        :successors
        (list (make-ev-charger
               :id 1004
               :initial-soc  92.0
               :soc-upper   100.0
               :rated-upper 22000.0)))

       ;; CHP modeled as a constant -2 kW generator on its meter.
       (make-meter :id 1007 :power -2000.0 :successors (list (make-chp :id 1006)))

       ;; Hidden consumer meter — invisible in ListComponents / tree
       ;; but aggregated into the main meter. `%make-meter` bypasses
       ;; meter-defaults so the explicit :power isn't combined with
       ;; a default :stream-jitter-pct on a hidden component. Power
       ;; follows a sine wave: peak 30 kW, trough 5 kW, one cycle
       ;; every 15 min, plus ±500 W jitter.
       (%make-meter
        :id 100 :name "consumer" :hidden t
        :power (lambda ()
                 (+ 17500.0
                    (* 12500.0 (sin (* 6.2831853 (/ (window-elapsed 900.0) 900.0))))
                    (- (random 1000) 500))))))))
   ;; Apply UI-driven edits the user has clicked Persist on. Loaded
   ;; from inside the :topology lambda so (current-microgrid-id)
   ;; resolves to 2200 and the overrides land in *this* microgrid's
   ;; site. The journal lives under <state-dir>/microgrids/.
   (load-overrides)))

;; -----------------------------------------------------------------------------
;; Scenarios — appear in the Scenarios mode dropdown; run one with
;; (scenario-run "<name>") or from the UI. All address the pinned
;; component ids above.
;; -----------------------------------------------------------------------------

;; Consumer load ramps through the evening peak; PV is gone, the
;; battery has to discharge to cover. Compressed to one transition a
;; minute so it plays in the UI without waiting for the wall clock.
(define-scenario
 :name "peak-evening-load"
 :description "Consumer ramp → peak → wind-down, PV gone, batteries discharging"
 :schedule 'relative
 :length "3min"
 :setup (lambda ()
          (set-meter-power 100 12000.0)
          (set-solar-sunlight 200 10.0))
 :cues (list
        (at "60s" (lambda ()
                    (set-meter-power 100 25000.0)
                    (set-solar-sunlight 200 0.0)
                    (set-active-power 1001 -10000.0)))
        (at "120s" (lambda ()
                     (set-meter-power 100 6000.0)
                     (set-active-power 1001 0.0)))))

;; Cloud bank crosses the array: full sun, dropout to near-overcast,
;; recovery. Useful for verifying a control app tracks the PV
;; envelope down + back up without overshooting.
(define-scenario
 :name "pv-dropout"
 :description "Clear → cloud bank → clear"
 :schedule 'relative
 :length "3min"
 :setup (lambda () (set-solar-sunlight 200 80.0))
 :cues (list
        (at "60s" (lambda () (set-solar-sunlight 200 15.0)))
        (at "120s" (lambda () (set-solar-sunlight 200 80.0)))))

;; Battery 1000 cycles ok / error. A control app targeting
;; BatteryPool::power should see the pool's bounds shrink while the
;; battery is sidelined and recover when it comes back ok.
(define-scenario
 :name "battery-degraded-fleet"
 :description "Battery 1000 flips ok/error"
 :schedule 'relative
 :length "4min"
 :setup (lambda () (set-component-health 1000 'ok))
 :cues (list
        (at "60s" (lambda () (set-component-health 1000 'error)))
        (at "120s" (lambda () (set-component-health 1000 'ok)))
        (at "180s" (lambda () (set-component-health 1000 'error)))))

;; Solar inverter loses telemetry + commands time out during a
;; midday window. Exercises a control app's resilience to partial
;; visibility without actually dropping packets.
;;
;; Restore helper. Setting a mode back to 'normal errors when the
;; config forbids it: the component's operational mode may deny
;; telemetry or control, and an 'error health keeps the command
;; channel shut. Tolerate the rejection — the component then just
;; stays as the config dictates, and the scenario keeps running.
(defun flaky-network-restore ()
  (condition-case nil (set-component-telemetry-mode 200 'normal) (error nil))
  (condition-case nil (set-component-command-mode 200 'normal) (error nil)))

(define-scenario
 :name "flaky-network"
 :description "Solar inverter goes silent + commands time out, then recovers"
 :schedule 'relative
 :length "3min"
 :setup (lambda () (flaky-network-restore))
 :cues (list
        (at "60s" (lambda ()
                    (set-component-telemetry-mode 200 'silent)
                    (set-component-command-mode 200 'timeout)))
        (at "120s" (lambda () (flaky-network-restore)))))

;; Grid frequency leans toward ±100 mHz, then releases back to the
;; base OU drift. Each cue shifts the OU process's nominal — the
;; driver keeps integrating and noise stays on, so the trace reads
;; like a real grid leaning toward the new operating point rather
;; than snapping to a constant.
(define-scenario
 :name "frequency-deviation"
 :description "Grid frequency leans ±100 mHz, then released"
 :schedule 'relative
 :length "4min"
 :setup (lambda () (override-frequency-model :nominal 49.9))
 :cues (list
        (at "60s" (lambda () (override-frequency-model :nominal 50.0)))
        (at "120s" (lambda () (override-frequency-model :nominal 50.1)))
        (at "180s" (lambda () (clear-frequency-override)))))

;; Every commandable component starts in standby. The battery
;; inverter comes online first, then solar, then EV + CHP. A control
;; app that polls health should observe its addressable set growing.
(define-scenario
 :name "cold-start"
 :description "Every component starts standby; gradual come-online"
 :schedule 'relative
 :length "4min"
 :setup (lambda ()
          (set-component-health 1001 'standby)
          (set-component-health 200 'standby)
          (set-component-health 1006 'standby)
          (set-component-health 1004 'standby))
 :cues (list
        (at "60s" (lambda () (set-component-health 1001 'ok)))
        (at "120s" (lambda () (set-component-health 200 'ok)))
        (at "180s" (lambda ()
                     (set-component-health 1006 'ok)
                     (set-component-health 1004 'ok)))))

;; Grid connection point goes 'error, simulating a forced islanding
;; event. PV + battery have to carry the load alone; the control app
;; should respond by clamping consumer setpoints and discharging the
;; battery.
(define-scenario
 :name "off-grid-island"
 :description "Grid goes 'error; PV + battery carry load alone, then reconnect"
 :schedule 'relative
 :length "3min"
 :setup (lambda () (set-component-health 1 'ok))
 :cues (list
        (at "60s" (lambda ()
                    (set-component-health 1 'error)
                    (set-active-power 1001 -10000.0)))
        (at "120s" (lambda ()
                     (set-component-health 1 'ok)
                     (set-active-power 1001 0.0)))))
