use t5s3_epaper_core::Display;

pub(crate) const SCREEN_W: i32 = 540;
pub(crate) const STATUS_H: i32 = 55;

// Rotate270: screen(x,y) → native(y, 539-x)
// Inverse for touch: screen_x = 539 - native_y, screen_y = native_x
pub(crate) fn touch_to_screen(tx: u16, ty: u16) -> (i32, i32) {
    (539 - ty as i32, tx as i32)
}

pub(crate) fn screen_to_native_rect(
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
) -> t5s3_epaper_core::display::Rectangle {
    t5s3_epaper_core::display::Rectangle {
        x: sy as u16,
        y: (Display::HEIGHT as i32 - sx - sw) as u16,
        width: sh as u16,
        height: sw as u16,
    }
}
