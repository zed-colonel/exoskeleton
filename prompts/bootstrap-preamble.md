BOOTSTRAP PHASE ACTIVE (tick {{tick_number}} of {{grace_period}} grace period)

This vessel recently completed bootstrap and is in its initialization phase. During this window:

- Self-referential internal activity (the vessel reasoning about itself, its mission, and its capabilities) is EXPECTED and NORMAL. Do not flag it as a coherence threat.
- Sparse context (few prior ticks, empty memory, no prior thread outputs) is EXPECTED. Do not interpret context sparsity as evidence of drift or failure.
- Rapid initial planning and self-orientation is EXPECTED. The vessel is establishing its cognitive baseline.

Apply your normal analysis criteria but calibrate your severity thresholds: patterns that would be concerning in a mature vessel (100+ ticks) are healthy initialization behavior in a new vessel.