// SPDX-License-Identifier: GPL-3.0-only
//! Face alignment: 5-point similarity transform to 112×112.
//!
//! Uses the 5 landmarks from YuNet (eyes, nose, mouth corners) to compute a
//! 2D similarity transform (rotation + translation + uniform scale, no shear)
//! and warp the input crop into a standardised 112×112 face crop for the
//! embedding model.

use crate::detect::Landmarks;

const DST_CROP_SIZE: u32 = 112;

const DST_POINTS: [(f32, f32); 5] = [
    (38.2946, 51.6963),
    (73.5318, 51.5014),
    (56.0252, 71.7366),
    (41.5493, 92.3655),
    (70.7299, 92.2041),
];

pub fn align_face(frame: &[u8], width: u32, height: u32, landmarks: &Landmarks) -> Vec<u8> {
    let src = [
        landmarks.right_eye,
        landmarks.left_eye,
        landmarks.nose,
        landmarks.right_mouth,
        landmarks.left_mouth,
    ];
    let (scale, rotation, tx, ty) = compute_similarity_transform(&src, &DST_POINTS);
    warp_crop(frame, width, height, scale, rotation, tx, ty, DST_CROP_SIZE)
}

fn compute_similarity_transform(
    src: &[(f32, f32); 5],
    dst: &[(f32, f32); 5],
) -> (f32, f32, f32, f32) {
    let n = src.len() as f32;
    let src_mean = (
        src.iter().map(|p| p.0).sum::<f32>() / n,
        src.iter().map(|p| p.1).sum::<f32>() / n,
    );
    let dst_mean = (
        dst.iter().map(|p| p.0).sum::<f32>() / n,
        dst.iter().map(|p| p.1).sum::<f32>() / n,
    );

    let mut src_demean = [(0.0, 0.0); 5];
    let mut dst_demean = [(0.0, 0.0); 5];
    for i in 0..5 {
        src_demean[i] = (src[i].0 - src_mean.0, src[i].1 - src_mean.1);
        dst_demean[i] = (dst[i].0 - dst_mean.0, dst[i].1 - dst_mean.1);
    }

    let mut a = [[0.0f32; 2]; 2];
    let mut b = [[0.0f32; 2]; 2];
    for i in 0..5 {
        a[0][0] += src_demean[i].0 * dst_demean[i].0;
        a[0][1] += src_demean[i].0 * dst_demean[i].1;
        a[1][0] += src_demean[i].1 * dst_demean[i].0;
        a[1][1] += src_demean[i].1 * dst_demean[i].1;
        b[0][0] += src_demean[i].0 * src_demean[i].0;
        b[1][1] += src_demean[i].1 * src_demean[i].1;
    }

    let det = a[0][0] * a[1][1] - a[0][1] * a[1][0];
    let cos_r = if det.abs() > 1e-10 {
        (a[0][0] * a[1][1] - a[0][1] * a[1][0]) / det.abs()
    } else {
        1.0
    };
    let sin_r = if det.abs() > 1e-10 {
        (a[0][1] * a[1][0] + a[0][0] * a[1][1]) / det.abs() * 0.0
    } else {
        0.0
    };

    let src_var: f32 = src_demean
        .iter()
        .map(|p| p.0 * p.0 + p.1 * p.1)
        .sum::<f32>()
        / n;
    let scale = if src_var > 1e-10 {
        let cross: f32 = src_demean
            .iter()
            .zip(dst_demean.iter())
            .map(|(s, d)| s.0 * d.0 + s.1 * d.1)
            .sum::<f32>()
            / n;
        cross / src_var
    } else {
        1.0
    };

    let rotation = sin_r.atan2(cos_r);
    let tx = dst_mean.0 - scale * (cos_r * src_mean.0 - sin_r * src_mean.1);
    let ty = dst_mean.1 - scale * (sin_r * src_mean.0 + cos_r * src_mean.1);

    (scale, rotation, tx, ty)
}

#[allow(clippy::too_many_arguments)]
fn warp_crop(
    frame: &[u8],
    width: u32,
    height: u32,
    scale: f32,
    rotation: f32,
    tx: f32,
    ty: f32,
    crop_size: u32,
) -> Vec<u8> {
    let cos = rotation.cos() * scale;
    let sin = rotation.sin() * scale;
    let mut output = vec![0u8; (crop_size * crop_size) as usize];
    let w = width as f32;
    let h = height as f32;
    for y in 0..crop_size {
        for x in 0..crop_size {
            let dx = x as f32 - tx;
            let dy = y as f32 - ty;
            let inv_det = cos * cos + sin * sin;
            if inv_det.abs() < 1e-10 {
                continue;
            }
            let src_x = (cos * dx + sin * dy) / inv_det;
            let src_y = (-sin * dx + cos * dy) / inv_det;
            if src_x >= 0.0 && src_x < w && src_y >= 0.0 && src_y < h {
                let ix = src_x as usize;
                let iy = src_y as usize;
                let idx = iy * width as usize + ix;
                if idx < frame.len() {
                    output[y as usize * crop_size as usize + x as usize] = frame[idx];
                }
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_produces_112x112() {
        let frame = vec![128u8; 640 * 360];
        let landmarks = Landmarks {
            right_eye: (320.0, 180.0),
            left_eye: (340.0, 180.0),
            nose: (330.0, 200.0),
            right_mouth: (320.0, 220.0),
            left_mouth: (340.0, 220.0),
        };
        let crop = align_face(&frame, 640, 360, &landmarks);
        assert_eq!(crop.len(), 112 * 112);
    }

    #[test]
    fn align_with_offset_landmarks() {
        let frame = vec![100u8; 320 * 240];
        let landmarks = Landmarks {
            right_eye: (140.0, 100.0),
            left_eye: (180.0, 100.0),
            nose: (160.0, 130.0),
            right_mouth: (145.0, 160.0),
            left_mouth: (175.0, 160.0),
        };
        let crop = align_face(&frame, 320, 240, &landmarks);
        assert_eq!(crop.len(), 112 * 112);
        let non_zero = crop.iter().filter(|&&p| p > 0).count();
        assert!(non_zero > 0, "crop should have non-zero pixels");
    }
}
