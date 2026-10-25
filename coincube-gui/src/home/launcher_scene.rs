//! The native Launcher's bounded spin. SVG keeps the projected Cube in the
//! same Iced scene as the floor; readable controls are ordinary widgets laid
//! over its square face once the turn ends.

use std::{
    fmt::Write,
    time::{Duration, Instant},
};

use super::CubeCatalogKey;
use crate::chain::{ChainId, ChainIdExt};

pub(super) const TURN: Duration = Duration::from_millis(820);
const HANDOFF: Duration = Duration::from_millis(160);
const PEERS: Duration = Duration::from_millis(500);
const REST_SIZE: f32 = 152.0;
const FACE_SIZE: f32 = 420.0;
const CAMERA_DISTANCE: f32 = 1100.0;
// Perspective projection of the front plane is exactly 420 logical pixels.
const FOCUSED_SIZE: f32 = FACE_SIZE * CAMERA_DISTANCE / (CAMERA_DISTANCE + FACE_SIZE / 2.0);

#[derive(Clone, Copy)]
pub(super) struct FaceText<'a> {
    pub name: &'a str,
    pub backup: &'a str,
    pub connect: &'a str,
    pub wallets: &'a str,
    pub passkey: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Motion {
    pub previous: Option<CubeCatalogKey>,
    started: Instant,
}

impl Motion {
    pub fn new(previous: Option<CubeCatalogKey>) -> Self {
        Self {
            previous,
            started: Instant::now(),
        }
    }

    pub fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started)
            >= TURN
                + if self.previous.is_some() {
                    HANDOFF
                } else {
                    Duration::ZERO
                }
    }

    pub fn progress(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started);
        let delay = if self.previous.is_some() {
            HANDOFF
        } else {
            Duration::ZERO
        };
        ease(elapsed.saturating_sub(delay).as_secs_f32() / TURN.as_secs_f32())
    }

    pub fn outgoing_progress(&self, now: Instant) -> f32 {
        1.0 - ease(now.saturating_duration_since(self.started).as_secs_f32() / TURN.as_secs_f32())
    }

    pub fn peer_opacity(&self, now: Instant) -> f32 {
        if self.previous.is_some() {
            return 0.4;
        }
        let t = (now.saturating_duration_since(self.started).as_secs_f32() / PEERS.as_secs_f32())
            .clamp(0.0, 1.0);
        1.0 - 0.6 * ease(t)
    }
}

// cubic-bezier(.22, 1, .36, 1), inverted from x to y so timing matches CSS.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t == 0.0 || t == 1.0 {
        return t;
    }
    let mut lo: f32 = 0.0;
    let mut hi: f32 = 1.0;
    for _ in 0..16 {
        let u = (lo + hi) * 0.5;
        let x = 3.0 * (1.0 - u).powi(2) * u * 0.22 + 3.0 * (1.0 - u) * u * u * 0.36 + u.powi(3);
        if x < t {
            lo = u;
        } else {
            hi = u;
        }
    }
    let u = (lo + hi) * 0.5;
    3.0 * (1.0 - u).powi(2) * u + 3.0 * (1.0 - u) * u * u + u.powi(3)
}

#[derive(Clone, Copy)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

impl Vec3 {
    const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    fn turn(self, pitch: f32, yaw: f32) -> Self {
        let (sx, cx) = pitch.sin_cos();
        let (sy, cy) = yaw.sin_cos();
        let y = self.y * cx - self.z * sx;
        let z = self.y * sx + self.z * cx;
        Self::new(self.x * cy + z * sy, y, -self.x * sy + z * cy)
    }
}

struct Face {
    points: [Vec3; 4],
    normal: Vec3,
    shade: &'static str,
}

const FACES: [Face; 6] = [
    Face {
        points: [
            Vec3::new(-0.5, -0.5, 0.5),
            Vec3::new(0.5, -0.5, 0.5),
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(-0.5, 0.5, 0.5),
        ],
        normal: Vec3::new(0.0, 0.0, 1.0),
        shade: "front",
    },
    Face {
        points: [
            Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(-0.5, -0.5, -0.5),
            Vec3::new(-0.5, 0.5, -0.5),
            Vec3::new(0.5, 0.5, -0.5),
        ],
        normal: Vec3::new(0.0, 0.0, -1.0),
        shade: "panel",
    },
    Face {
        points: [
            Vec3::new(-0.5, -0.5, -0.5),
            Vec3::new(-0.5, -0.5, 0.5),
            Vec3::new(-0.5, 0.5, 0.5),
            Vec3::new(-0.5, 0.5, -0.5),
        ],
        normal: Vec3::new(-1.0, 0.0, 0.0),
        shade: "left",
    },
    Face {
        points: [
            Vec3::new(0.5, -0.5, 0.5),
            Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(0.5, 0.5, -0.5),
            Vec3::new(0.5, 0.5, 0.5),
        ],
        normal: Vec3::new(1.0, 0.0, 0.0),
        shade: "panel",
    },
    Face {
        points: [
            Vec3::new(-0.5, 0.5, 0.5),
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(0.5, 0.5, -0.5),
            Vec3::new(-0.5, 0.5, -0.5),
        ],
        normal: Vec3::new(0.0, 1.0, 0.0),
        shade: "top",
    },
    Face {
        points: [
            Vec3::new(-0.5, -0.5, -0.5),
            Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(0.5, -0.5, 0.5),
            Vec3::new(-0.5, -0.5, 0.5),
        ],
        normal: Vec3::new(0.0, -1.0, 0.0),
        shade: "left",
    },
];

fn paint_cube(
    svg: &mut String,
    cx: f32,
    cy: f32,
    progress: f32,
    network: ChainId,
    light: bool,
    target_yaw: f32,
    text: Option<FaceText<'_>>,
) {
    let size = REST_SIZE + (FOCUSED_SIZE - REST_SIZE) * progress;
    let pitch = 35.0_f32.to_radians() * (1.0 - progress);
    let yaw = (45.0 + (target_yaw - 45.0) * progress).to_radians();
    let edge = if network.is_blake2b() {
        "#d4a62a"
    } else if matches!(network, ChainId::Bitcoin) {
        "#f3821d"
    } else {
        "#b9bbb5"
    };
    let colors = if light {
        ["#c9c1b2", "#e6e0d5", "#d6cfc2", "#e6e0d5"]
    } else {
        ["#262724", "#2a2b28", "#2e2f2c", "#41423e"]
    };
    let mut faces = Vec::with_capacity(3);
    for (index, face) in FACES.iter().enumerate() {
        if face.normal.turn(pitch, yaw).z <= 0.001 {
            continue;
        }
        let vertices = face.points.map(|point| point.turn(pitch, yaw));
        let depth = vertices.iter().map(|point| point.z).sum::<f32>() / 4.0;
        faces.push((depth, index, face.shade, vertices));
    }
    faces.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, index, shade, vertices) in faces {
        let color = match shade {
            "front" => colors[0],
            "panel" => colors[1],
            "left" => colors[2],
            _ => colors[3],
        };
        let _ = write!(svg, "<polygon points=\"");
        let projected = vertices.map(|point| {
            let depth = CAMERA_DISTANCE / (CAMERA_DISTANCE - point.z * size);
            (cx + point.x * size * depth, cy - point.y * size * depth)
        });
        for (x, y) in projected {
            let _ = write!(svg, "{x:.2},{y:.2} ");
        }
        let dash = if matches!(network, ChainId::Bitcoin | ChainId::BitcoinBlake2b) {
            ""
        } else {
            " stroke-dasharray=\"6 4\""
        };
        let _ = write!(
            svg,
            "\" fill=\"{color}\" stroke=\"{edge}\" stroke-width=\"2\"{dash}/>"
        );
        if index == 1 && progress > 0.5 {
            if let Some(text) = text {
                paint_face_text(svg, projected, network, light, text);
            }
        }
        if index == 3 && target_yaw > 180.0 {
            if let Some(text) = text {
                paint_unlock_text(svg, projected, light, text);
            }
        }
    }
}

fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn paint_face_text(
    svg: &mut String,
    p: [(f32, f32); 4],
    network: ChainId,
    light: bool,
    text: FaceText<'_>,
) {
    // The projected back plane supplies an affine approximation while it
    // turns. At rest the live Iced controls replace this snapshot exactly.
    let tl = p[3];
    let tr = p[2];
    let bl = p[0];
    let a = (tr.0 - tl.0) / FACE_SIZE;
    let b = (tr.1 - tl.1) / FACE_SIZE;
    let c = (bl.0 - tl.0) / FACE_SIZE;
    let d = (bl.1 - tl.1) / FACE_SIZE;
    let ink = if light { "#202020" } else { "#f5f0e8" };
    let muted = if light { "#575a54" } else { "#b9bbb5" };
    let name: String = text.name.chars().take(25).collect();
    let _ = write!(svg, "<g transform=\"matrix({a:.5} {b:.5} {c:.5} {d:.5} {:.2} {:.2})\" font-family=\"sans-serif\" fill=\"{ink}\">", tl.0, tl.1);
    let _ = write!(
        svg,
        "<text x=\"28\" y=\"51\" font-size=\"20\" font-weight=\"600\">{}</text>",
        xml_text(&name)
    );
    let _ = write!(
        svg,
        "<text x=\"28\" y=\"79\" font-size=\"14\" fill=\"{muted}\">{}</text>",
        xml_text(network.label())
    );
    let _ = write!(
        svg,
        "<path d=\"M28 105H392\" stroke=\"{muted}\" stroke-width=\"1\"/>"
    );
    for (y, label, value) in [
        (137, "Lock", "State unknown"),
        (171, "Backup", text.backup),
        (205, "Connect", text.connect),
        (239, "Wallets", text.wallets),
    ] {
        let value: String = value.chars().take(28).collect();
        let _ = write!(svg, "<text x=\"28\" y=\"{y}\" font-size=\"13\" fill=\"{muted}\">{label}</text><text x=\"111\" y=\"{y}\" font-size=\"14\" fill=\"{ink}\">{}</text>", xml_text(&value));
    }
    svg.push_str("</g>");
}

fn paint_unlock_text(svg: &mut String, p: [(f32, f32); 4], light: bool, text: FaceText<'_>) {
    let tl = p[3];
    let tr = p[2];
    let bl = p[0];
    let a = (tr.0 - tl.0) / FACE_SIZE;
    let b = (tr.1 - tl.1) / FACE_SIZE;
    let c = (bl.0 - tl.0) / FACE_SIZE;
    let d = (bl.1 - tl.1) / FACE_SIZE;
    let ink = if light { "#202020" } else { "#f5f0e8" };
    let muted = if light { "#575a54" } else { "#b9bbb5" };
    let name: String = text.name.chars().take(20).collect();
    let _ = write!(svg, "<g transform=\"matrix({a:.5} {b:.5} {c:.5} {d:.5} {:.2} {:.2})\" font-family=\"sans-serif\" fill=\"{ink}\">", tl.0, tl.1);
    let _ = write!(
        svg,
        "<text x=\"28\" y=\"54\" font-size=\"20\" font-weight=\"600\">Open {}</text>",
        xml_text(&name)
    );
    if text.passkey {
        let _ = write!(svg, "<text x=\"28\" y=\"133\" font-size=\"14\" fill=\"{muted}\">Confirm with your passkey</text><rect x=\"28\" y=\"164\" width=\"214\" height=\"38\" rx=\"6\" fill=\"#f3821d\"/><text x=\"43\" y=\"190\" font-size=\"14\" fill=\"#201407\">Unlock with passkey</text>");
    } else {
        let _ = write!(
            svg,
            "<text x=\"28\" y=\"130\" font-size=\"14\" fill=\"{muted}\">PIN</text>"
        );
        for x in [28, 86, 144, 202] {
            let _ = write!(svg, "<rect x=\"{x}\" y=\"147\" width=\"48\" height=\"48\" rx=\"7\" fill=\"none\" stroke=\"{muted}\"/>");
        }
        let _ = write!(svg, "<rect x=\"28\" y=\"226\" width=\"108\" height=\"38\" rx=\"6\" fill=\"#f3821d\"/><text x=\"44\" y=\"251\" font-size=\"14\" fill=\"#201407\">Unlock</text>");
    }
    svg.push_str("</g>");
}

pub(super) fn source(
    width: f32,
    height: f32,
    start: (f32, f32),
    target_y: f32,
    network: ChainId,
    text: FaceText<'_>,
    light: bool,
    progress: f32,
    outgoing: Option<((f32, f32), ChainId, FaceText<'_>, f32)>,
) -> String {
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width:.2} {height:.2}\">"
    );
    if let Some((previous_start, previous_network, previous_text, previous_progress)) = outgoing {
        let x = previous_start.0 + (width * 0.5 - previous_start.0) * previous_progress;
        let y = previous_start.1 + (target_y - previous_start.1) * previous_progress;
        paint_cube(
            &mut svg,
            x,
            y,
            previous_progress,
            previous_network,
            light,
            180.0,
            Some(previous_text),
        );
    }
    let x = start.0 + (width * 0.5 - start.0) * progress;
    let y = start.1 + (target_y - start.1) * progress;
    paint_cube(&mut svg, x, y, progress, network, light, 180.0, Some(text));
    svg.push_str("</svg>");
    svg
}

/// The second hidden face faces the camera while the real PIN or passkey
/// controls sit in its square. The existing unlock reducers retain authority.
pub(super) fn unlock_source(network: ChainId, light: bool) -> String {
    let mut svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 420 420\">".to_owned();
    paint_cube(&mut svg, 210.0, 210.0, 1.0, network, light, 270.0, None);
    svg.push_str("</svg>");
    svg
}

pub(super) fn second_turn_progress(started: Instant, now: Instant) -> f32 {
    ease(now.saturating_duration_since(started).as_secs_f32() / TURN.as_secs_f32())
}

/// The details back face leaves the camera as the adjacent unlock face arrives.
/// The PIN/passkey controls mount as soon as this bounded turn completes.
pub(super) fn second_turn_source(
    network: ChainId,
    light: bool,
    text: FaceText<'_>,
    progress: f32,
) -> String {
    let mut svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 420 420\">".to_owned();
    paint_cube(
        &mut svg,
        210.0,
        210.0,
        1.0,
        network,
        light,
        180.0 + 90.0 * progress.clamp(0.0, 1.0),
        Some(text),
    );
    svg.push_str("</svg>");
    svg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_face_has_exact_projected_size_and_bounded_motion() {
        let projected = FOCUSED_SIZE * CAMERA_DISTANCE / (CAMERA_DISTANCE - FOCUSED_SIZE / 2.0);
        assert!((projected - FACE_SIZE).abs() < 0.01);
        let motion = Motion::new(None);
        assert!(motion.progress(motion.started) < 0.001);
        assert!(motion.done(motion.started + TURN));
        assert!((motion.progress(motion.started + TURN) - 1.0).abs() < 0.001);
    }

    #[test]
    fn handoff_reverses_the_old_cube_before_the_new_cube_turns() {
        let motion = Motion::new(Some(CubeCatalogKey {
            chain: ChainId::Bitcoin,
            cube_id: "old".into(),
        }));
        assert_eq!(motion.progress(motion.started + HANDOFF / 2), 0.0);
        assert!(motion.outgoing_progress(motion.started + HANDOFF / 2) < 1.0);
        assert_eq!(motion.peer_opacity(motion.started), 0.4);
        assert!(!motion.done(motion.started + TURN));
        assert!(motion.done(motion.started + TURN + HANDOFF));
    }

    #[test]
    fn turning_details_face_escapes_cube_names_and_unlock_uses_next_face() {
        let details = source(
            1000.0,
            740.0,
            (250.0, 265.0),
            340.0,
            ChainId::Bitcoin,
            FaceText {
                name: "Alice & <Bob>",
                backup: "Recorded",
                connect: "Signed in",
                wallets: "Vault",
                passkey: false,
            },
            false,
            1.0,
            None,
        );
        assert!(details.contains("Alice &amp; &lt;Bob&gt;"));
        assert!(!details.contains("Alice & <Bob>"));
        assert!(details.contains("Recorded"));
        let unlock = unlock_source(ChainId::Bitcoin, false);
        assert!(unlock.contains("viewBox=\"0 0 420 420\""));
        assert!(!unlock.contains("Alice"));
        let started = Instant::now();
        assert_eq!(second_turn_progress(started, started), 0.0);
        assert_eq!(second_turn_progress(started, started + TURN), 1.0);
        let half = second_turn_progress(started, started + TURN / 2);
        assert!(half > 0.0 && half < 1.0);
        let face = FaceText {
            name: "Alice & <Bob>",
            backup: "Recorded",
            connect: "Signed in",
            wallets: "Vault",
            passkey: false,
        };
        assert!(second_turn_source(ChainId::Bitcoin, false, face, 0.0)
            .contains("Alice &amp; &lt;Bob&gt;"));
        let turned = second_turn_source(ChainId::Bitcoin, false, face, 1.0);
        assert!(turned.contains("Open Alice &amp; &lt;Bob&gt;"));
        assert!(turned.contains(">PIN</text>"));
    }
}
