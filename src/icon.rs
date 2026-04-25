pub const APP_ICON_SIZE: usize = 64;

pub fn app_icon_rgba() -> Vec<u8> {
    let size = APP_ICON_SIZE;
    let mut rgba = Vec::with_capacity(size * size * 4);
    let mut canvas = vec![[0u8; 4]; size * size];

    let clear = [0, 0, 0, 0];
    let ink = [22, 33, 45, 255];
    let paper = [247, 243, 235, 255];
    let accent = [255, 191, 68, 255];
    let peak = [14, 122, 138, 255];
    let peak_dark = [7, 74, 92, 255];

    fill_rounded_rect(&mut canvas, size, 9, 18, 46, 30, 7, ink);
    fill_rounded_rect(&mut canvas, size, 14, 10, 36, 12, 6, ink);
    fill_rounded_rect(&mut canvas, size, 17, 13, 16, 4, 2, accent);
    fill_rounded_rect(&mut canvas, size, 26, 9, 14, 6, 3, paper);

    fill_rounded_rect(&mut canvas, size, 6, 22, 52, 26, 5, ink);
    fill_rounded_rect(&mut canvas, size, 10, 26, 44, 18, 4, paper);
    fill_circle(&mut canvas, size, 45, 34, 5, accent);

    fill_triangle(&mut canvas, size, (12, 44), (26, 29), (40, 44), peak);
    fill_triangle(&mut canvas, size, (33, 44), (43, 34), (52, 44), peak_dark);

    fill_rounded_rect(&mut canvas, size, 9, 42, 46, 6, 3, ink);
    fill_rounded_rect(&mut canvas, size, 10, 43, 44, 4, 2, paper);

    for pixel in canvas {
        let px = if pixel[3] == 0 { clear } else { pixel };
        rgba.extend_from_slice(&px);
    }

    rgba
}

fn fill_rounded_rect(
    canvas: &mut [[u8; 4]],
    size: usize,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
    color: [u8; 4],
) {
    for py in y..(y + height) {
        for px in x..(x + width) {
            if !in_bounds(size, px, py) {
                continue;
            }

            let dx = if px < x + radius {
                x + radius - px
            } else if px >= x + width - radius {
                px - (x + width - radius - 1)
            } else {
                0
            };
            let dy = if py < y + radius {
                y + radius - py
            } else if py >= y + height - radius {
                py - (y + height - radius - 1)
            } else {
                0
            };

            if dx == 0 || dy == 0 || dx * dx + dy * dy <= radius * radius {
                canvas[(py as usize * size) + px as usize] = color;
            }
        }
    }
}

fn fill_circle(canvas: &mut [[u8; 4]], size: usize, cx: i32, cy: i32, radius: i32, color: [u8; 4]) {
    let radius_sq = radius * radius;
    for py in (cy - radius)..=(cy + radius) {
        for px in (cx - radius)..=(cx + radius) {
            if in_bounds(size, px, py) && (px - cx) * (px - cx) + (py - cy) * (py - cy) <= radius_sq
            {
                canvas[(py as usize * size) + px as usize] = color;
            }
        }
    }
}

fn fill_triangle(
    canvas: &mut [[u8; 4]],
    size: usize,
    a: (i32, i32),
    b: (i32, i32),
    c: (i32, i32),
    color: [u8; 4],
) {
    let min_x = a.0.min(b.0).min(c.0);
    let max_x = a.0.max(b.0).max(c.0);
    let min_y = a.1.min(b.1).min(c.1);
    let max_y = a.1.max(b.1).max(c.1);

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            if in_bounds(size, px, py) && point_in_triangle((px, py), a, b, c) {
                canvas[(py as usize * size) + px as usize] = color;
            }
        }
    }
}

fn point_in_triangle(p: (i32, i32), a: (i32, i32), b: (i32, i32), c: (i32, i32)) -> bool {
    let area = |p1: (i32, i32), p2: (i32, i32), p3: (i32, i32)| {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    };

    let d1 = area(p, a, b);
    let d2 = area(p, b, c);
    let d3 = area(p, c, a);
    let has_neg = d1 < 0 || d2 < 0 || d3 < 0;
    let has_pos = d1 > 0 || d2 > 0 || d3 > 0;

    !(has_neg && has_pos)
}

fn in_bounds(size: usize, x: i32, y: i32) -> bool {
    x >= 0 && y >= 0 && x < size as i32 && y < size as i32
}
