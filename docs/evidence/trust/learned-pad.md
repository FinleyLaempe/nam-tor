# MiniFASNet Learned PAD Integration
This document describes the integration of the MiniFASNet model for learned PAD.
The model was trained on a dataset of physics-based synthetic strobe-differential data.
It captures high-dimensional signals of the differential energy falloff and specular hotspots.
Integration into the `namtor-daemon` occurs at the final gate, where it provides a final veto on the liveness assessment.
The model is deployed in a zero-trust worker process.
