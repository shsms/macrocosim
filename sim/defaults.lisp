;; macrocosim per-category defaults + bare-name shorthand DSL.
;;
;; Part of the embedded prelude (compiled into the binary via
;; include_str!), so editing this file needs a rebuild. A script
;; that wants live defaults-editing can `(load "sim/defaults.lisp")`
;; and `(watch-file …)` it explicitly.
;;
;; Two halves:
;;
;;   1. `*-defaults` plists hold the category-wide knobs. Edit a
;;      value here to retune every component of that category in one
;;      place. Per-component plist args still win on each call.
;;
;;   2. `make-*` wrappers (`make-grid-connection-point`,
;;      `make-meter`, `make-battery`, …) are thin `defuns` that call
;;      the matching `%make-*` Rust primitive with the defaults plist
;;      appended *after* the caller's args. The first occurrence of a
;;      key wins, as with `plist-get`, so the per-component plist
;;      overrides category defaults; the `%make-*` primitives stay
;;      available for callers that want zero defaults.

;; -----------------------------------------------------------------------------
;; Per-category defaults
;; -----------------------------------------------------------------------------

(setq grid-defaults
      '(:rated-fuse-current-a 100
        :stream-jitter-pct  1.0))

(setq meter-defaults
      '(:interval-s        0.2
        :stream-jitter-pct 4.0))

(setq battery-defaults
      '(:soc-protect-margin-pct 10.0
        :stream-jitter-pct  8.0
        :health             ok))

(setq battery-inverter-defaults
      '(:command-delay-s      1.5
        :ramp-rate-w-per-s 5000.0
        :stream-jitter-pct     8.0
        :reactive-pf-limit     0.0         ;; 0 = disabled
        :reactive-apparent-va 32000.0))    ;; kVA-circle envelope

;; Deliberately NO :sunlight-pct here — an omitted :sunlight-pct is what
;; makes an array follow the site weather, so adding one to this
;; plist would pin every solar inverter site-wide to a manual
;; constant and silently disable (make-weather).
(setq solar-inverter-defaults
      '(:ramp-rate-w-per-s 2000.0
        :stream-jitter-pct  5.0))

(setq ev-charger-defaults
      '(:command-delay-s     0.5
        :ramp-rate-w-per-s 3000.0
        :stream-jitter-pct   10.0))

;; Steam boiler: hybrid gas/electric — the electric side ramps and
;; delays like the EV charger.
(setq steam-boiler-defaults
      '(:command-delay-s 0.5
        :ramp-rate-w-per-s 50000.0
        :stream-jitter-pct 10.0))

;; The marker categories (chp, wind turbine, power transformer,
;; breaker) carry no physics — one shared default plist.
(setq marker-defaults
      '(:stream-jitter-pct 0.0))

;; -----------------------------------------------------------------------------
;; make-* shorthand wrappers
;; -----------------------------------------------------------------------------
;;
;; Each wrapper appends its `<cat>-defaults` plist after the caller's
;; args. The first occurrence of a key wins, as with `plist-get`, so
;; the per-component plist overrides the defaults. To bypass defaults
;; entirely for one call, call the `%make-*` primitive directly:
;;
;;   (%make-battery :id 100)                       ; no defaults

(defun make-grid-connection-point (&rest args)
  "Build a grid connection point and return its handle.

It calls %make-grid-connection-point with ARGS, then the keys in
grid-defaults, so a key in ARGS wins over the same key in
grid-defaults.

Keys:
  :id  component id, a positive integer; default: the next free id
  :name  display name
  :rated-fuse-current-a  fuse rating in A, not negative
  :rated-lower-w  lower end of the rated band in W; give both ends or neither
  :rated-upper-w  upper end of the rated band in W; without both, no band
  :successors  list of handles of the components below it
  :stream-jitter-pct  random spread of each telemetry interval, in percent
  :operational-mode, :health, :telemetry-mode, :command-mode
    the modes it starts in, as the set-component-* functions take them"
  (apply '%make-grid-connection-point (append args grid-defaults)))

(defun make-meter (&rest args)
  "Build a meter and return its handle.

It calls %make-meter with ARGS, then the keys in meter-defaults, so a
key in ARGS wins over the same key in meter-defaults.

Keys:
  :id  component id, a positive integer; default: the next free id
  :name  display name
  :interval-s  telemetry interval in seconds
  :power-w  active power in W: a number, a lambda or a symbol; default: the sum of its successors
  :reactive-power-var  reactive power in VAr: a number, a lambda or a symbol; not with :power-factor
  :power-factor  power factor in (0, 1] that sets Q from the meter's own P; not with :reactive-power-var
  :leading  t for a leading Q, which has the opposite sign of P; needs :power-factor
  :successors  list of handles of the components below it
  :hidden  t to leave it out of the Microgrid API's component lists; its parent still counts its power
  :stream-jitter-pct  random spread of each telemetry interval, in percent
  :operational-mode, :health, :telemetry-mode, :command-mode
    the modes it starts in, as the set-component-* functions take them"
  (apply '%make-meter (append args meter-defaults)))

(defun make-battery (&rest args)
  "Build a battery and return its handle.

It calls %make-battery with ARGS, then the keys in battery-defaults,
so a key in ARGS wins over the same key in battery-defaults.

Keys:
  :id  component id, a positive integer; default: the next free id
  :name  display name
  :interval-s  telemetry interval in seconds; default 1
  :capacity-wh  energy it holds when full, in Wh; default 92000
  :initial-soc-pct  state of charge at the start, in percent; default 50
  :soc-lower-pct  lower end of the SoC window, in percent; default 10
  :soc-upper-pct  upper end of the SoC window, in percent; default 90
  :soc-protect-margin-pct  room inside each SoC limit where the bounds taper to 0, in percent
  :voltage-v  DC voltage in V; default 800
  :rated-lower-w  lower end of the rated band in W; default -30000
  :rated-upper-w  upper end of the rated band in W; default 30000
  :stream-jitter-pct  random spread of each telemetry interval, in percent
  :operational-mode, :health, :telemetry-mode, :command-mode
    the modes it starts in, as the set-component-* functions take them"
  (apply '%make-battery (append args battery-defaults)))

(defun make-battery-inverter (&rest args)
  "Build a battery inverter and return its handle.

It calls %make-battery-inverter with ARGS, then the keys in
battery-inverter-defaults, so a key in ARGS wins over the same key in
battery-inverter-defaults.

Keys:
  :id  component id, a positive integer; default: the next free id
  :name  display name
  :interval-s  telemetry interval in seconds; default 1
  :successors  list of handles of its batteries
  :rated-lower-w  lower end of the rated band in W; default -30000
  :rated-upper-w  upper end of the rated band in W; default 30000
  :command-delay-s  delay before a command starts, in seconds
  :ramp-rate-w-per-s  how fast the active power moves, in W/s
  :device-delay-s  time a command takes to reach the output, in seconds; default 0.1
  :reactive-pf-limit  k in |Q| <= k * |P|; 0 or less turns it off
  :reactive-apparent-va  limit on P^2 + Q^2 <= VA^2; 0 or less turns it off
  :reactive-command-delay-s  delay before a Q command starts, in seconds; default 0.1
  :reactive-ramp-rate-var-per-s  how fast the reactive power moves, in VAr/s; default 2000
  :stream-jitter-pct  random spread of each telemetry interval, in percent
  :operational-mode, :health, :telemetry-mode, :command-mode
    the modes it starts in, as the set-component-* functions take them

With both reactive limits off, the inverter still keeps |Q| <= |P|."
  (apply '%make-battery-inverter (append args battery-inverter-defaults)))

(defun make-solar-inverter (&rest args)
  "Build a solar inverter and return its handle.

It calls %make-solar-inverter with ARGS, then the keys in
solar-inverter-defaults, so a key in ARGS wins over the same key in
solar-inverter-defaults.

Keys:
  :id  component id, a positive integer; default: the next free id
  :name  display name
  :interval-s  telemetry interval in seconds; default 1
  :sunlight-pct  sunlight in percent: a number, a lambda or a symbol; leave it out to follow the site weather
  :rated-lower-w  lower end of the rated band in W; default -30000
  :rated-upper-w  upper end of the rated band in W; default 0
  :array-peak-w  peak DC output of the panels in W, positive; default: the size of :rated-lower-w
  :weather-lag-s  how far behind the site weather it reads, 0 to 3600 seconds; default: 0 to 60 s, from the id
  :weather-jitter-pct  random spread of each weather reading, 0 to under 100 percent; default 0
  :command-delay-s  delay before a command starts, in seconds; default 0
  :ramp-rate-w-per-s  how fast the active power moves, in W/s
  :device-delay-s  time a command takes to reach the output, in seconds; default 0.1
  :reactive-pf-limit  k in |Q| <= k * |P|; 0 or less turns it off; default 0.35
  :reactive-apparent-va  limit on P^2 + Q^2 <= VA^2; 0 or less turns it off; default off
  :reactive-command-delay-s  delay before a Q command starts, in seconds; default 0.1
  :reactive-ramp-rate-var-per-s  how fast the reactive power moves, in VAr/s; default 2000
  :stream-jitter-pct  random spread of each telemetry interval, in percent
  :operational-mode, :health, :telemetry-mode, :command-mode
    the modes it starts in, as the set-component-* functions take them

With both reactive limits off, the inverter still keeps |Q| <= |P|."
  (apply '%make-solar-inverter (append args solar-inverter-defaults)))

(defun make-ev-charger (&rest args)
  "Build an EV charger and return its handle.

It calls %make-ev-charger with ARGS, then the keys in
ev-charger-defaults, so a key in ARGS wins over the same key in
ev-charger-defaults. The charger starts with no car; plug one in with
plug-ev.

Keys:
  :id  component id, a positive integer; default: the next free id
  :name  display name
  :interval-s  telemetry interval in seconds; default 1
  :rated-lower-w  lower end of the rated band in W; default 0
  :rated-upper-w  upper end of the rated band in W; default 22000
  :command-delay-s  delay before a command starts, in seconds
  :ramp-rate-w-per-s  how fast the offered limit moves, in W/s
  :device-delay-s  time a command takes to reach the output, in seconds; default 0.1
  :phases  1 or 3, the phases it is wired on; default 3
  :idle  what it offers with no command: 'paused (nothing) or 'full (its whole rating); default 'paused
  :resume-on-recovery  t to keep the command through a health fault; default nil
  :stream-jitter-pct  random spread of each telemetry interval, in percent
  :operational-mode, :health, :telemetry-mode, :command-mode
    the modes it starts in, as the set-component-* functions take them

The battery keys :capacity-wh, :initial-soc-pct, :soc-lower-pct,
:soc-upper-pct and :soc-protect-margin-pct are ignored with a warning:
the pack belongs to the car (see plug-ev)."
  (apply '%make-ev-charger (append args ev-charger-defaults)))

(defun make-steam-boiler (&rest args)
  "Build a steam boiler and return its handle.

It calls %make-steam-boiler with ARGS, then the keys in
steam-boiler-defaults, so a key in ARGS wins over the same key in
steam-boiler-defaults.

Keys:
  :id  component id, a positive integer; default: the next free id
  :name  display name
  :interval-s  telemetry interval in seconds; default 1
  :demand-kg-per-s  steam demand in kg/s: a number, a lambda or a symbol; default 0
  :rated-lower-w  lower end of the rated band in W; default 0
  :rated-upper-w  upper end of the rated band in W; default 250000
  :target-bar  pressure the gas burner holds, in bar; above 0; default 8
  :max-bar  highest pressure, in bar; at least :target-bar; default 10
  :initial-bar  pressure at the start, in bar; default: :target-bar
  :capacity-wh-per-bar  energy to raise the pressure by 1 bar, in Wh; default 10000
  :wh-per-kg  energy to make 1 kg of steam, in Wh; default 627
  :command-delay-s  delay before a command starts, in seconds
  :ramp-rate-w-per-s  how fast the electric power moves, in W/s
  :device-delay-s  time a command takes to reach the heater, in seconds; default 0.1
  :stream-jitter-pct  random spread of each telemetry interval, in percent
  :operational-mode, :health, :telemetry-mode, :command-mode
    the modes it starts in, as the set-component-* functions take them"
  (apply '%make-steam-boiler (append args steam-boiler-defaults)))

(defun make-chp (&rest args)
  "Build a CHP and return its handle.

It has no physics of its own. It completes the topology and sets the
kind of the meters next to it. To give it power, set the power of a
neighboring meter with set-meter-power.

It calls %make-chp with ARGS, then the keys in marker-defaults, so a
key in ARGS wins over the same key in marker-defaults.

Keys:
  :id  component id, a positive integer; default: the next free id
  :name  display name
  :stream-jitter-pct  random spread of each telemetry interval, in percent
  :operational-mode, :health, :telemetry-mode, :command-mode
    the modes it starts in, as the set-component-* functions take them"
  (apply '%make-chp (append args marker-defaults)))

(defun make-wind-turbine (&rest args)
  "Build a wind turbine and return its handle.

It has no physics of its own. It completes the topology and sets the
kind of the meters next to it. To give it power, set the power of a
neighboring meter with set-meter-power.

It calls %make-wind-turbine with ARGS, then the keys in
marker-defaults, so a key in ARGS wins over the same key in
marker-defaults.

Keys:
  :id  component id, a positive integer; default: the next free id
  :name  display name
  :stream-jitter-pct  random spread of each telemetry interval, in percent
  :operational-mode, :health, :telemetry-mode, :command-mode
    the modes it starts in, as the set-component-* functions take them"
  (apply '%make-wind-turbine (append args marker-defaults)))

(defun make-power-transformer (&rest args)
  "Build a power transformer and return its handle.

It has no physics of its own. It completes the topology and sets the
kind of the meters next to it.

It calls %make-power-transformer with ARGS, then the keys in
marker-defaults, so a key in ARGS wins over the same key in
marker-defaults.

Keys:
  :id  component id, a positive integer; default: the next free id
  :name  display name
  :stream-jitter-pct  random spread of each telemetry interval, in percent
  :operational-mode, :health, :telemetry-mode, :command-mode
    the modes it starts in, as the set-component-* functions take them"
  (apply '%make-power-transformer (append args marker-defaults)))

(defun make-breaker (&rest args)
  "Build a breaker and return its handle.

It has no physics of its own. It completes the topology and sets the
kind of the meters next to it.

It calls %make-breaker with ARGS, then the keys in marker-defaults,
so a key in ARGS wins over the same key in marker-defaults.

Keys:
  :id  component id, a positive integer; default: the next free id
  :name  display name
  :stream-jitter-pct  random spread of each telemetry interval, in percent
  :operational-mode, :health, :telemetry-mode, :command-mode
    the modes it starts in, as the set-component-* functions take them"
  (apply '%make-breaker (append args marker-defaults)))
