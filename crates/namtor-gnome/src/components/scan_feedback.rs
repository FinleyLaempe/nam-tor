// SPDX-License-Identifier: GPL-3.0-only
//! Shared scan-feedback widgets for the enrollment capture and review pages.
//!
//! There is deliberately **no camera preview** — raw frames never leave the
//! daemon. Everything the user needs in order to judge their own scan has to
//! come from the handful of coarse numbers on the `org.namtor.Manager3` wire
//! (`quality_percent`, `quality_bucket`, `diversity_percent`, the frame
//! counters and a typed guidance token). This module turns those into the
//! two things a face scan has to answer at a glance:
//!
//! 1. *What do I do right now?* — the guidance headline, the largest text on
//!    the page, with the silhouette above it tinted by the quality bucket.
//! 2. *How is it going, and why?* — three separately measured signals, each
//!    with its own reading, plus the one signal the daemon says is currently
//!    limiting the scan.
//!
//! Styling uses libadwaita's own classes only (`DESIGN.md` §8 forbids custom
//! CSS), and every string goes through [`crate::tr`].

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::ActionRowExt;

use crate::tr;
use crate::view_model::GuidanceView;

/// Edge length of the silhouette frame, in logical pixels. Large enough to
/// read as the stand-in for a viewfinder, small enough that the guidance
/// headline below it still fits above the fold at phone width.
const SILHOUETTE_FRAME_PX: i32 = 148;
const SILHOUETTE_ICON_PX: i32 = 84;

/// Width reserved for an inline meter bar. Narrow enough that an action row
/// keeps its title on one line down to a 360 px window.
const METER_BAR_PX: i32 = 84;

/// libadwaita style class for a quality bucket (0 = low, 1 = medium,
/// 2 = high). The bucket is the daemon's own verdict, so the UI never has to
/// invent a threshold — or, worse, match a translated word.
fn bucket_class(bucket: u8) -> &'static str {
    match bucket {
        b if b >= 2 => "success",
        1 => "dim-label",
        _ => "warning",
    }
}

/// Translated one-word verdict for a quality bucket.
fn bucket_label(bucket: u8) -> String {
    tr(match bucket {
        b if b >= 2 => "Good",
        1 => "Fair",
        _ => "Poor",
    })
}

/// The face silhouette: a framed placeholder that stands in for the camera
/// preview the app deliberately does not have. Its tint follows the quality
/// bucket, so the frame itself reports whether the camera can currently see
/// a usable face.
pub fn silhouette(quality_bucket: u8) -> gtk4::Widget {
    let icon = gtk4::Image::from_icon_name("avatar-default-symbolic");
    icon.set_pixel_size(SILHOUETTE_ICON_PX);
    icon.set_css_classes(&[bucket_class(quality_bucket)]);
    icon.set_halign(gtk4::Align::Center);
    icon.set_valign(gtk4::Align::Center);
    icon.set_vexpand(true);
    icon.set_hexpand(true);

    let frame = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .css_classes(["card"])
        .width_request(SILHOUETTE_FRAME_PX)
        .height_request(SILHOUETTE_FRAME_PX)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .vexpand(false)
        .build();
    frame.append(&icon);
    frame.set_accessible_role(gtk4::AccessibleRole::Img);
    frame.update_property(&[
        gtk4::accessible::Property::Label(&tr("Face position indicator")),
        gtk4::accessible::Property::Description(&tr(
            "A silhouette standing in for the camera view. No video is shown.",
        )),
    ]);
    frame.upcast()
}

/// The headline plus its one-line "how". This is the only actionable element
/// on the capture page, so it is the largest text on it.
///
/// `encouraging` and `quality_bucket` are typed inputs — nothing here looks
/// at the text of the headline, so the styling survives translation.
pub fn headline_block(
    headline_text: &str,
    encouraging: bool,
    detail: Option<String>,
    quality_bucket: u8,
) -> gtk4::Widget {
    let headline = gtk4::Label::builder()
        .label(headline_text)
        .justify(gtk4::Justification::Center)
        .wrap(true)
        .max_width_chars(24)
        .halign(gtk4::Align::Center)
        .build();
    if encouraging || quality_bucket >= 2 {
        headline.set_css_classes(&["title-1", "success"]);
    } else {
        headline.set_css_classes(&["title-1"]);
    }
    headline.set_accessible_role(gtk4::AccessibleRole::Status);
    headline.update_property(&[
        gtk4::accessible::Property::Label(&tr("Scan guidance")),
        gtk4::accessible::Property::Description(&tr("What to do right now to improve the scan")),
    ]);

    let column = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(6)
        .halign(gtk4::Align::Center)
        .build();
    column.append(&headline);

    if let Some(detail) = detail {
        let sub = gtk4::Label::builder()
            .label(detail)
            .justify(gtk4::Justification::Center)
            .wrap(true)
            .max_width_chars(38)
            .halign(gtk4::Align::Center)
            .css_classes(["dim-label"])
            .build();
        sub.set_accessible_role(gtk4::AccessibleRole::Note);
        column.append(&sub);
    }

    column.upcast()
}

/// Hero block for an arbitrary headline: silhouette above the headline.
pub fn hero_with(
    headline_text: &str,
    encouraging: bool,
    detail: Option<String>,
    quality_bucket: u8,
) -> gtk4::Widget {
    let column = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(18)
        .halign(gtk4::Align::Center)
        .margin_top(6)
        .margin_bottom(12)
        .build();
    column.append(&silhouette(quality_bucket));
    column.append(&headline_block(
        headline_text,
        encouraging,
        detail,
        quality_bucket,
    ));
    column.upcast()
}

/// Hero block driven by the daemon's guidance: silhouette above the guidance
/// headline and its one-line "how".
pub fn hero(guidance: &GuidanceView, quality_bucket: u8) -> gtk4::Widget {
    hero_with(
        &guidance.label(),
        guidance.is_encouraging(),
        guidance.hint(),
        quality_bucket,
    )
}

/// One measured signal: a titled row with a plain-language explanation, a
/// percentage and an inline bar.
///
/// `value_class` is a libadwaita style class applied to the number, or `None`
/// for the default. Only signals the daemon itself judges (the quality
/// bucket) are tinted — progress towards a goal is left neutral so the one
/// tinted row on the page always means "this is what to fix".
pub fn meter_row(
    title: &str,
    subtitle: &str,
    percent: u32,
    value_class: Option<&str>,
    accessible_name: &str,
) -> adw::ActionRow {
    let clamped = percent.min(100);

    let value = gtk4::Label::builder()
        .label(format!("{clamped}%"))
        .valign(gtk4::Align::Center)
        .build();
    let mut classes = vec!["numeric"];
    if let Some(extra) = value_class {
        classes.push(extra);
    } else {
        classes.push("dim-label");
    }
    value.set_css_classes(&classes);

    let bar = gtk4::ProgressBar::builder()
        .fraction(f64::from(clamped) / 100.0)
        .valign(gtk4::Align::Center)
        .width_request(METER_BAR_PX)
        .build();
    bar.set_accessible_role(gtk4::AccessibleRole::ProgressBar);
    bar.update_property(&[gtk4::accessible::Property::Label(&tr(accessible_name))]);

    let suffix = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(10)
        .valign(gtk4::Align::Center)
        .build();
    suffix.append(&value);
    suffix.append(&bar);

    let row = adw::ActionRow::builder()
        .title(tr(title))
        .subtitle(tr(subtitle))
        .subtitle_lines(0)
        .build();
    row.add_suffix(&suffix);
    row.update_property(&[
        gtk4::accessible::Property::Label(&tr(accessible_name)),
        gtk4::accessible::Property::Description(&format!("{}%", clamped)),
    ]);
    row
}

/// One honest counter: a titled row with a plain-language explanation and the
/// raw number. Nothing here is a percentage, so nothing is rounded away.
pub fn count_row(title: &str, subtitle: &str, value: u32, accessible_name: &str) -> adw::ActionRow {
    let number = gtk4::Label::builder()
        .label(value.to_string())
        .valign(gtk4::Align::Center)
        .css_classes(["numeric", "dim-label"])
        .build();

    let row = adw::ActionRow::builder()
        .title(tr(title))
        .subtitle(tr(subtitle))
        .subtitle_lines(0)
        .build();
    row.add_suffix(&number);
    row.update_property(&[
        gtk4::accessible::Property::Label(&tr(accessible_name)),
        gtk4::accessible::Property::Description(&value.to_string()),
    ]);
    row
}

/// The row that names the signal currently limiting the scan.
///
/// The per-signal measurements themselves are daemon-internal and never cross
/// the D-Bus contract, but the guidance token *is* the daemon's own statement
/// of which signal it is unhappy with. Naming it here is the honest way to
/// answer "why is the quality low?" without inventing numbers.
pub fn attention_row(guidance: &GuidanceView) -> adw::ActionRow {
    let (value_text, value_class, subtitle) = match guidance.signal() {
        Some(signal) => (
            tr(signal.as_label()),
            "warning",
            tr("What the scan is waiting on"),
        ),
        None => (
            tr("Nothing"),
            "success",
            tr("Every signal is good enough right now"),
        ),
    };

    let value = gtk4::Label::builder()
        .label(&value_text)
        .valign(gtk4::Align::Center)
        .css_classes(["heading", value_class])
        .build();

    let row = adw::ActionRow::builder()
        .title(tr("Needs attention"))
        .subtitle(&subtitle)
        .subtitle_lines(0)
        .build();
    row.add_suffix(&value);
    row.set_accessible_role(gtk4::AccessibleRole::Status);
    row.update_property(&[
        gtk4::accessible::Property::Label(&tr("Signal needing attention")),
        gtk4::accessible::Property::Description(&value_text),
    ]);
    row
}

/// The "Image quality" meter, tinted by the daemon's own quality bucket.
pub fn image_quality_row(quality_percent: u32, quality_bucket: u8) -> adw::ActionRow {
    let row = meter_row(
        "Image quality",
        "How well the camera sees you",
        quality_percent,
        Some(bucket_class(quality_bucket)),
        "Image quality",
    );
    row.update_property(&[gtk4::accessible::Property::Description(&format!(
        "{}% — {}",
        quality_percent.min(100),
        bucket_label(quality_bucket)
    ))]);
    row
}

/// The "Angle variety" meter — pose diversity, the same measurement on the
/// capture page and the review page.
pub fn angle_variety_row(diversity_percent: u32) -> adw::ActionRow {
    meter_row(
        "Angle variety",
        "Head angles collected so far",
        diversity_percent,
        if diversity_percent >= 100 {
            Some("success")
        } else {
            None
        },
        "Angle variety",
    )
}

/// The "Views kept" meter — the share of observed captures the daemon judged
/// usable. Derived from two counters that are both on the wire, so it is a
/// measurement rather than a restatement of `quality_percent`.
pub fn kept_rate_row(percent: Option<u32>) -> adw::ActionRow {
    match percent {
        Some(p) => meter_row(
            "Views kept",
            "Usable share of what the camera sent",
            p,
            None,
            "Share of captured views kept",
        ),
        None => {
            let row = adw::ActionRow::builder()
                .title(tr("Views kept"))
                .subtitle(tr("Waiting for the first view"))
                .subtitle_lines(0)
                .build();
            let placeholder = gtk4::Label::builder()
                .label(tr("—"))
                .valign(gtk4::Align::Center)
                .css_classes(["numeric", "dim-label"])
                .build();
            row.add_suffix(&placeholder);
            row.update_property(&[gtk4::accessible::Property::Label(&tr(
                "Share of captured views kept",
            ))]);
            row
        }
    }
}
