# Analytic PAD Calibration
This document records the calibration of the analytic PAD (Physical Attack Detection) gates.
The gates (Differential Energy, Gradient Consistency, Specular Highlight, and Radial Falloff) are calibrated to detect non-human signals.
Calibration was performed against a baseline of 1000+ captures of human faces under controlled lighting conditions (standardized for 850nm IR).
Veto thresholds are set to flag any frame pair where the delta $D = On - Off$ exceeds the defined threshold across all four gates.
