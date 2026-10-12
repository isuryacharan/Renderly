# Bundled ML models

## face_detection_yunet_2026may.onnx

YuNet face detector, used by `AutoReframeClip` (`renderly-core/src/ml/yunet.rs`).

| | |
|---|---|
| Source | https://github.com/opencv/opencv_zoo/tree/26cc381e4d2594bb9f47a26eb8fd96c94a13660d/models/face_detection_yunet |
| Commit | `26cc381e4d2594bb9f47a26eb8fd96c94a13660d` (opencv/opencv_zoo, 2026-05-22) |
| File | `face_detection_yunet_2026may.onnx` (Git LFS object in the Zoo; dynamic H/W input) |
| Size | 229,738 bytes |
| SHA-256 | `ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0` |
| Licence | MIT, see `LICENSE-yunet-opencv-zoo` (copied verbatim from the Zoo's `models/face_detection_yunet/LICENSE`; the Zoo's README states the licence covers the model files) |
| Model origin | https://github.com/ShiqiYu/libfacedetection.train (YuNet; Wu et al., "YuNet: A Tiny Millisecond-level Face Detector") |

The same weights also exist as the fixed-640x640 `face_detection_yunet_2023mar.onnx`
(SHA-256 `8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4`, 232,589 bytes);
both load and produce identical outputs under tract. The dynamic variant is bundled because a
plan can be built for any 32-multiple input size.

`renderly-core/src/ml/models.rs` pins these values and a unit test re-hashes the embedded bytes.
