//! Thin constructors over CALayer and AppKit, in the design's vocabulary.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker, MainThreadOnly};
use objc2_core_graphics::CGColor;
use objc2_quartz_core::CAMediaTimingFunction;
use objc2_app_kit::{
    NSAnimationContext, NSColor, NSFont, NSTextField, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSNumber, NSString};

use compact_core::theme::Rgba;

/// A `CALayer`. Untyped because objc2 ships no QuartzCore bindings; every use
/// here is a property set, which `msg_send!` expresses directly.
pub type Layer = Retained<AnyObject>;

pub fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

/// A `CGColor` for a design token.
///
/// Layer color properties take `CGColorRef`, and `-[NSColor CGColor]` is the
/// documented bridge. Returning the `Retained` (rather than a raw pointer)
/// keeps it alive until the caller has handed it to a layer, which retains it.
pub fn cg_color(c: Rgba) -> Retained<CGColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.r, c.g, c.b, c.a).CGColor()
}

/// An empty layer.
pub fn layer() -> Layer {
    unsafe {
        let cls = objc2::runtime::AnyClass::get(c"CALayer").expect("CALayer must exist");
        let obj: *mut AnyObject = msg_send![cls, layer];
        Retained::retain(obj).expect("CALayer alloc")
    }
}

/// A rounded, optionally bordered rectangle — the design's basic building block.
pub fn panel(frame: CGRect, fill: Rgba, radius: f64, border: Option<(Rgba, f64)>) -> Layer {
    let l = layer();
    unsafe {
        let _: () = msg_send![&*l, setFrame: frame];
        let bg = cg_color(fill);
        let _: () = msg_send![&*l, setBackgroundColor: &*bg];
        let _: () = msg_send![&*l, setCornerRadius: radius];
        if let Some((bc, bw)) = border {
            let bc = cg_color(bc);
            let _: () = msg_send![&*l, setBorderColor: &*bc];
            let _: () = msg_send![&*l, setBorderWidth: bw];
        }
    }
    l
}

/// Horizontal alignment for [`text`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    fn name(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
        }
    }
}

/// A single-line `CATextLayer`.
///
/// Text layers are used rather than `NSTextField` for everything non-editable:
/// they are a fraction of the weight, they animate with their parent, and there
/// are a lot of them in a tab strip.
pub fn text(s: &str, size: f64, color: Rgba, mono: bool, align: Align) -> Layer {
    let l = unsafe {
        let cls = objc2::runtime::AnyClass::get(c"CATextLayer").expect("CATextLayer must exist");
        let obj: *mut AnyObject = msg_send![cls, layer];
        Retained::retain(obj).expect("CATextLayer alloc")
    };
    unsafe {
        let _: () = msg_send![&*l, setString: &*NSString::from_str(s)];
        let _: () = msg_send![&*l, setFontSize: size];
        let fgc = cg_color(color);
        let _: () = msg_send![&*l, setForegroundColor: &*fgc];
        let _: () = msg_send![&*l, setAlignmentMode: &*NSString::from_str(align.name())];
        let _: () = msg_send![&*l, setTruncationMode: &*NSString::from_str("end")];
        // Match the display so text is not rendered at 1x on a retina panel.
        let _: () = msg_send![&*l, setContentsScale: 2.0];
        // `CATextLayer.font` is a `CFTypeRef`, not an object pointer, so it
        // must be passed untyped. `NSFont` is toll-free bridged with
        // `CTFontRef`, which is what the layer wants — passing the family name
        // instead would lose the weight.
        let f = font(size, mono, false);
        let raw: *const c_void = Retained::as_ptr(&f).cast();
        let _: () = msg_send![&*l, setFont: raw];
    }
    l
}

/// Replace a text layer's string.
pub fn set_text(l: &Layer, s: &str) {
    unsafe {
        let _: () = msg_send![&**l, setString: &*NSString::from_str(s)];
    }
}

pub fn set_frame(l: &Layer, f: CGRect) {
    unsafe {
        let _: () = msg_send![&**l, setFrame: f];
    }
}

/// Set a layer's opacity.
pub fn set_opacity(l: &Layer, v: f64) {
    unsafe {
        let _: () = msg_send![&**l, setOpacity: v as f32];
    }
}

pub fn set_hidden(l: &Layer, v: bool) {
    unsafe {
        let _: () = msg_send![&**l, setHidden: v];
    }
}

pub fn set_fill(l: &Layer, c: Rgba) {
    unsafe {
        let c = cg_color(c);
        let _: () = msg_send![&**l, setBackgroundColor: &*c];
    }
}

pub fn set_border(l: &Layer, c: Rgba, w: f64) {
    unsafe {
        let c = cg_color(c);
        let _: () = msg_send![&**l, setBorderColor: &*c];
        let _: () = msg_send![&**l, setBorderWidth: w];
    }
}

pub fn set_color(l: &Layer, c: Rgba) {
    unsafe {
        let c = cg_color(c);
        let _: () = msg_send![&**l, setForegroundColor: &*c];
    }
}

pub fn add_sublayer(parent: &Layer, child: &Layer) {
    unsafe {
        let _: () = msg_send![&**parent, addSublayer: &**child];
    }
}

/// A horizontal two-stop gradient layer.
pub fn gradient(frame: CGRect, from: Rgba, to: Rgba) -> Layer {
    unsafe {
        let cls = objc2::runtime::AnyClass::get(c"CAGradientLayer").expect("CAGradientLayer");
        let obj: *mut AnyObject = msg_send![cls, layer];
        let l = Retained::retain(obj).expect("CAGradientLayer alloc");
        let _: () = msg_send![&*l, setFrame: frame];
        // `colors` wants CGColorRefs in an NSArray. They are CF types, not
        // ObjC objects, so they go in as raw pointers rather than through a
        // typed array.
        let a = cg_color(from);
        let b = cg_color(to);
        // Typed as `id` pointers, not `void*`: `arrayWithObjects:count:`
        // encodes its first argument as `^@` and objc2 checks that at runtime.
        let ptrs: [*const AnyObject; 2] = [
            Retained::as_ptr(&a).cast(),
            Retained::as_ptr(&b).cast(),
        ];
        let ns = objc2::runtime::AnyClass::get(c"NSArray").expect("NSArray");
        let colors: *mut AnyObject = msg_send![ns, arrayWithObjects: ptrs.as_ptr(), count: 2usize];
        let _: () = msg_send![&*l, setColors: colors];
        // Left to right.
        let _: () = msg_send![&*l, setStartPoint: CGPoint::new(0.0, 0.5)];
        let _: () = msg_send![&*l, setEndPoint: CGPoint::new(1.0, 0.5)];
        l
    }
}

/// Give a layer a coloured bloom.
pub fn glow(l: &Layer, color: Rgba, radius: f64) {
    unsafe {
        let c = cg_color(color);
        let _: () = msg_send![&**l, setShadowColor: &*c];
        let _: () = msg_send![&**l, setShadowRadius: radius];
        let _: () = msg_send![&**l, setShadowOpacity: 0.6f32];
        let _: () = msg_send![&**l, setShadowOffset: CGSize::new(0.0, 0.0)];
    }
}

/// A layer's first sublayer, if it has one.
pub fn first_sublayer(l: &Layer) -> Option<Layer> {
    unsafe {
        let subs: *mut AnyObject = msg_send![&**l, sublayers];
        if subs.is_null() {
            return None;
        }
        let n: usize = msg_send![subs, count];
        if n == 0 {
            return None;
        }
        let obj: *mut AnyObject = msg_send![subs, objectAtIndex: 0usize];
        Retained::retain(obj)
    }
}

/// Attach a layer as a view's backing layer content.
pub fn set_view_layer(view: &NSView, l: &Layer) {
    unsafe {
        view.setWantsLayer(true);
        let host: *mut AnyObject = msg_send![view, layer];
        let _: () = msg_send![host, addSublayer: &**l];
    }
}

/// Suppress CALayer's implicit animations for the duration of `f`.
///
/// Every property set on a layer animates over 0.25 s by default. That is wrong
/// for state sync — a tab title would cross-fade on every keystroke — so all
/// updates run inside this, and the transitions the design *does* want are
/// declared explicitly instead.
pub fn without_animation<R>(f: impl FnOnce() -> R) -> R {
    unsafe {
        let cls = objc2::runtime::AnyClass::get(c"CATransaction").expect("CATransaction");
        let _: () = msg_send![cls, begin];
        let _: () = msg_send![cls, setDisableActions: true];
        let r = f();
        let _: () = msg_send![cls, commit];
        r
    }
}

/// A design easing curve as a `CAMediaTimingFunction`.
pub fn timing(curve: [f32; 4]) -> Retained<CAMediaTimingFunction> {
    let [a, b, c, d] = curve;
    CAMediaTimingFunction::functionWithControlPoints(a, b, c, d)
}

/// The chrome easing curve.
pub fn ease() -> Retained<CAMediaTimingFunction> {
    timing(compact_core::theme::EASE_OUT_EXPO)
}

/// Animate AppKit view properties over `duration` with the design's easing.
///
/// Inside `f`, set properties on `view.animator()` rather than the view. This
/// is the supported path for `NSView` geometry and alpha — `NSVisualEffectView`
/// in particular manages its own backing layer, so driving its layer directly
/// fights AppKit's layout instead of animating it.
pub fn animate<R>(duration: f64, f: impl FnOnce() -> R) -> R {
    animate_with(compact_core::theme::EASE_OUT_EXPO, duration, f)
}

/// As [`animate`], on a specific easing curve.
pub fn animate_with<R>(curve: [f32; 4], duration: f64, f: impl FnOnce() -> R) -> R {
    NSAnimationContext::beginGrouping();
    let ctx = NSAnimationContext::currentContext();
    ctx.setDuration(duration);
    ctx.setTimingFunction(Some(&timing(curve)));
    let r = f();
    NSAnimationContext::endGrouping();
    r
}

/// Run `f` inside an explicit CATransaction with a duration and the design's
/// easing. For raw `CALayer` properties, which `NSAnimationContext` does not
/// cover.
pub fn animated<R>(duration: f64, f: impl FnOnce() -> R) -> R {
    unsafe {
        let cls = objc2::runtime::AnyClass::get(c"CATransaction").expect("CATransaction");
        let _: () = msg_send![cls, begin];
        let _: () = msg_send![cls, setAnimationDuration: duration];
        let _: () = msg_send![cls, setAnimationTimingFunction: &*ease()];
        let r = f();
        let _: () = msg_send![cls, commit];
        r
    }
}

/// Attach an infinitely repeating, auto-reversing opacity animation.
///
/// The canvas's `cpBreathe` keyframes on the onboarding hint.
pub fn breathe(l: &Layer, from: f64, to: f64, period: f64) {
    unsafe {
        let cls = objc2::runtime::AnyClass::get(c"CABasicAnimation").expect("CABasicAnimation");
        let key = NSString::from_str("opacity");
        let anim: *mut AnyObject = msg_send![cls, animationWithKeyPath: &*key];
        let _: () = msg_send![anim, setFromValue: &*NSNumber::new_f64(from)];
        let _: () = msg_send![anim, setToValue: &*NSNumber::new_f64(to)];
        // Half a period each way; auto-reverse supplies the return trip.
        let _: () = msg_send![anim, setDuration: period / 2.0];
        let _: () = msg_send![anim, setAutoreverses: true];
        let _: () = msg_send![anim, setRepeatCount: f32::INFINITY];
        let _: () = msg_send![anim, setTimingFunction: &*CAMediaTimingFunction::functionWithControlPoints(0.42, 0.0, 0.58, 1.0)];
        let _: () = msg_send![&**l, addAnimation: anim, forKey: &*NSString::from_str("breathe")];
    }
}

/// Horizontally offset a view's backing layer, in points.
///
/// Translation only. Rotation and scale would need the layer's `anchorPoint` at

/// Set a layer's vertical scale about its centre, for the address bar's squash.
pub fn set_scale_y(l: &Layer, sy: f64) {
    unsafe {
        let t = [1.0f64, 0.0, 0.0, 0.0, 0.0, sy, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let cls = objc2::runtime::AnyClass::get(c"NSValue").expect("NSValue");
        let v: *mut AnyObject = msg_send![cls,
            valueWithBytes: &t as *const _ as *const c_void,
            objCType: c"{CATransform3D=dddddddddddddddd}".as_ptr()];
        let _: () = msg_send![&**l, setValue: v, forKeyPath: &*NSString::from_str("transform")];
    }
}

/// A font in the design's stack, falling back to the system face when the
/// webfont is not installed — the canvas declares the same fallback.
pub fn font(size: f64, mono: bool, medium: bool) -> Retained<NSFont> {
    let family = if mono {
        compact_core::theme::font::MONO
    } else {
        compact_core::theme::font::UI
    };
    let name = NSString::from_str(family);
    let weight_name = if medium {
        NSString::from_str(&format!("{family} Medium"))
    } else {
        name.clone()
    };
    if let Some(f) = NSFont::fontWithName_size(&weight_name, size) {
        return f;
    }
    if let Some(f) = NSFont::fontWithName_size(&name, size) {
        return f;
    }
    if mono {
        NSFont::monospacedSystemFontOfSize_weight(size, if medium { 0.23 } else { 0.0 })
    } else if medium {
        NSFont::systemFontOfSize_weight(size, 0.23)
    } else {
        NSFont::systemFontOfSize(size)
    }
}

/// A blurred backdrop panel — the canvas's `backdrop-filter: blur(...)`.
pub fn blur_view(mtm: MainThreadMarker, frame: CGRect, radius: f64) -> Retained<NSVisualEffectView> {
    unsafe {
        let v = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
        v.setMaterial(NSVisualEffectMaterial::HUDWindow);
        v.setBlendingMode(NSVisualEffectBlendingMode::WithinWindow);
        // Keep the blur live even when the window is not key; chrome that goes
        // flat on focus loss reads as a rendering bug.
        v.setState(NSVisualEffectState::Active);
        v.setWantsLayer(true);
        let l: *mut AnyObject = msg_send![&*v, layer];
        let _: () = msg_send![l, setCornerRadius: radius];
        let _: () = msg_send![l, setMasksToBounds: true];
        v
    }
}

/// A borderless, transparent editable field — the address bar and palette query.
pub fn field(mtm: MainThreadMarker, frame: CGRect, size: f64, mono: bool, color: Rgba) -> Retained<NSTextField> {
    unsafe {
        let f = NSTextField::initWithFrame(NSTextField::alloc(mtm), frame);
        f.setBezeled(false);
        f.setDrawsBackground(false);
        f.setBordered(false);
        f.setFocusRingType(objc2_app_kit::NSFocusRingType::None);
        f.setFont(Some(&font(size, mono, false)));
        f.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            color.r, color.g, color.b, color.a,
        )));
        // The design shows raw URLs and command text; autocorrect would mangle
        // both.
        let _: () = msg_send![&*f, setAutomaticTextCompletionEnabled: false];
        f
    }
}

/// Plain container view with a backing layer.
pub fn container(mtm: MainThreadMarker, frame: CGRect) -> Retained<NSView> {
    let v = NSView::initWithFrame(NSView::alloc(mtm), frame);
    v.setWantsLayer(true);
    v
}

/// Set a view's backing-layer background. Backing layers come back from AppKit
/// as untyped pointers, so they get their own helpers rather than going through
/// [`Layer`].
pub fn set_host_fill(host: *mut AnyObject, c: Rgba) {
    let c = cg_color(c);
    unsafe {
        let _: () = msg_send![host, setBackgroundColor: &*c];
    }
}

/// Set a view's backing-layer border.
pub fn set_host_border(host: *mut AnyObject, c: Rgba, w: f64) {
    let c = cg_color(c);
    unsafe {
        let _: () = msg_send![host, setBorderColor: &*c];
        let _: () = msg_send![host, setBorderWidth: w];
    }
}
