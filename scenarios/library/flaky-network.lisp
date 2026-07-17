;; Solar inverter loses telemetry + commands time out during a midday
;; window. Exercises a control app's resilience to partial visibility —
;; a real flaky-link experience without having to actually drop packets.
;;
;; A relative demo: goes flaky at +60s, recovers at +120s.

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
