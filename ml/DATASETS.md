# Datasets

**Required by spec §10.1.** Read this before touching training data.

## The rule

**No dataset and no dataset-derived weights are ever committed to this repository.**

`ml/` ships *recipes*, not weights. Anyone who accepts a dataset's terms runs the recipe and obtains their own weights. Measured results are published without redistributing anything.

This is not caution for its own sake. Essentially every usable face dataset is research-licensed and non-redistributable, several prohibit commercial use, and at least one has been withdrawn by its publisher. An open-source project that ships weights derived from them is distributing something it has no right to distribute.

## Verification requirement

**Verify a dataset's current terms yourself before use.** The table below records what was understood at the time of writing and is not a substitute for reading the licence. Terms change, and access procedures change more often.

When you verify one, update its row with the date and what you confirmed.

---

## Face recognition — NIR

For the embedding fine-tune (spec §10.2).

| Dataset | Scale | Licence status | Access | Verified |
|---|---|---|---|---|
| **CASIA NIR-VIS 2.0** | ~725 subjects, ~17.5k images | Research use, signed agreement, non-commercial | Application to CASIA / CBSR | — |
| **LAMP-HQ** | ~570 subjects, ~57k NIR | Research licence | Author request | — |
| **BUAA-VisNir** | ~150 subjects | Research licence | Author request | — |
| **Oulu-CASIA NIR-VIS** | ~80 subjects | Research licence | University of Oulu | — |

**The primary value of CASIA NIR-VIS 2.0 is its paired VIS/NIR images of the same subjects**, which enable the distillation approach in spec §10.2 — training the NIR encoder to match a frozen VIS encoder's embedding of the paired image. That needs *pairs*, not identity diversity, which is precisely the axis on which these small datasets are adequate.

725 identities against MS1M's ~85,000 means full fine-tuning would overfit and cause catastrophic forgetting. Adapt into the feature space; do not retrain it.

## Face recognition — visible light

Not used directly. Listed because pretrained weights derive from them, and because their status determines what may be shipped.

| Dataset | Status | Note |
|---|---|---|
| **MS-Celeb-1M (MS1M)** | **Withdrawn by Microsoft.** Scraped without consent. | **Do not use, do not download, do not depend on.** Widely used in published research regardless; that does not make it available to us. |
| **Glint360K** | Research licence | Derived partly from withdrawn sources |
| **WebFace260M** | Research licence, registration | |
| **VGGFace2** | Withdrawn from official distribution | |

## Presentation attack detection

For the learned PAD model (spec §10.3).

| Dataset | Modalities | Licence status | Note |
|---|---|---|---|
| **CASIA-SURF** | RGB, depth, IR | Research licence | IR present, but **not our modality** |
| **WMCA (Idiap)** | RGB, depth, IR, thermal | Research licence, signed agreement | As above |
| **CASIA-SURF CeFA** | RGB, depth, IR | Research licence | Cross-ethnicity coverage |

**None of these contain what we need.** Our PAD input is the strobe *differential* `D = On − Off` from our own emitter geometry. No public dataset contains that modality, from any sensor.

This is why spec §10.3 sequences analytic PAD first — physics-based gates need zero training data and already reject screen replay. The learned model follows once `namtor-collect` has gathered real differential data.

## Pretrained weights

Fetched at first run under the upstream licence. Never vendored.

| Source | Licence | Note |
|---|---|---|
| **InsightFace** model zoo (`buffalo_*`) | Code MIT; **several models are non-commercial research use** | Check per model, not per repository |
| **AdaFace** released weights | Per upstream repository | Primary embedding backbone (spec §6.3) |
| **YuNet** (OpenCV Zoo) | Permissive | Primary face detector, chosen partly for this |
| **MiniFASNet / Silent-Face** | Code Apache-2.0; **weights trained on non-permissive data** | Reference only; we train our own on differentials |

**Licence status of code and of weights differ constantly.** An MIT repository routinely ships weights under a research-only licence. Check both.

## Score normalisation cohort — open item

Spec §6.6 requires a background cohort of embeddings for per-user score normalisation. A cohort derived from licence-restricted datasets is legally ambiguous **even as bare vectors** — this is tracked as risk R5.

Preferred sources, in order:

1. **Synthetic cohort** — generated faces, no provenance problem
2. **Contributed cohort** — collected under consent via `namtor-collect`
3. Derived-vector cohort from a research dataset, only with an explicit legal read

Do not default to option 3 because it is easiest.

## Self-collected data

Gathered by `namtor-collect` (spec §10.4). Rules:

- **Local-only by default.** Nothing leaves the machine without explicit upload.
- **Explicit informed consent** at collection time — not a checkbox buried in a licence.
- **Prefer derived features and differential statistics over raw face images** wherever they suffice. Most PAD signal survives the reduction.
- Per-session identifiers, so withdrawal and deletion are actually possible.
- Consented fixtures may be committed to `fixtures/` **only** with the consent record retained.

## Honest limitation

A PAD model trained on a handful of subjects and one sensor family **will not generalise like Microsoft's**, which is validated across certified hardware with laboratory-scale attack batteries.

The eval report states this rather than obscuring it. Gate G4 is reported per attack type and per device class, with sample sizes, so the limitation is visible in the numbers rather than buried in prose.
