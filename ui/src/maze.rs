//! A tilt maze on a half-block pixel grid: each terminal cell is two square-ish pixels.
use alloc::{collections::VecDeque, vec, vec::Vec};

use ratatui::{buffer::Buffer, layout::Rect, style::Color, widgets::Widget};

/// Maze cells are 4x4 px corridors separated by 1 px walls.
const PITCH: usize = 5;
const MID: f32 = PITCH as f32 / 2.0;
const GRAVITY: f32 = 90.0; // px/s^2 per g of tilt
const DAMPING: f32 = 0.985;
const TRAIL: usize = 40;

pub struct Maze {
    pub cols: usize,
    pub rows: usize,
    width: usize,
    height: usize,
    walls: Vec<bool>,
    pub ball: (f32, f32),
    vel: (f32, f32),
    trail: VecDeque<(usize, usize)>,
    path: Vec<(usize, usize)>,
}

impl Maze {
    pub fn new(cols: usize, rows: usize, seed: u32) -> Self {
        let (width, height) = (cols * PITCH + 1, rows * PITCH + 1);
        let mut maze = Self {
            cols,
            rows,
            width,
            height,
            walls: vec![true; width * height],
            ball: (MID, MID),
            vel: (0.0, 0.0),
            trail: VecDeque::new(),
            path: Vec::new(),
        };
        maze.generate(seed);
        maze
    }

    /// Size in terminal cells.
    pub fn size(&self) -> (u16, u16) {
        (self.width as u16, self.height.div_ceil(2) as u16)
    }

    fn generate(&mut self, mut seed: u32) {
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut visited = vec![false; self.cols * self.rows];
        let mut stack = vec![(0usize, 0usize)];
        visited[0] = true;
        self.carve_cell(0, 0);
        while let Some(&(cx, cy)) = stack.last() {
            let mut options = [(0usize, 0usize); 4];
            let mut n = 0;
            for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (cx as i32 + dx, cy as i32 + dy);
                if nx >= 0 && ny >= 0 && (nx as usize) < self.cols && (ny as usize) < self.rows {
                    let (nx, ny) = (nx as usize, ny as usize);
                    if !visited[ny * self.cols + nx] {
                        options[n] = (nx, ny);
                        n += 1;
                    }
                }
            }
            if n == 0 {
                stack.pop();
                continue;
            }
            let (nx, ny) = options[rng() as usize % n];
            visited[ny * self.cols + nx] = true;
            self.carve_cell(nx, ny);
            self.carve_between((cx, cy), (nx, ny));
            stack.push((nx, ny));
        }
    }

    fn carve_cell(&mut self, cx: usize, cy: usize) {
        for y in 0..PITCH - 1 {
            for x in 0..PITCH - 1 {
                let (px, py) = (cx * PITCH + 1 + x, cy * PITCH + 1 + y);
                self.walls[py * self.width + px] = false;
            }
        }
    }

    fn carve_between(&mut self, a: (usize, usize), b: (usize, usize)) {
        let (x0, y0) = (a.0.min(b.0) * PITCH + 1, a.1.min(b.1) * PITCH + 1);
        let (w, h) = if a.0 != b.0 { (PITCH, PITCH - 1) } else { (PITCH - 1, PITCH) };
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                self.walls[y * self.width + x] = false;
            }
        }
    }

    fn wall(&self, x: f32, y: f32) -> bool {
        if x < 0.0 || y < 0.0 {
            return true;
        }
        let (x, y) = (x as usize, y as usize);
        x >= self.width || y >= self.height || self.walls[y * self.width + x]
    }

    pub fn cell(&self) -> (usize, usize) {
        (self.ball.0 as usize / PITCH, self.ball.1 as usize / PITCH)
    }

    pub fn goal(&self) -> (usize, usize) {
        (self.cols - 1, self.rows - 1)
    }

    pub fn solved(&self) -> bool {
        self.cell() == self.goal()
    }

    /// Roll the ball under a tilt in g (screen axes: +x right, +y down).
    pub fn roll(&mut self, tilt: (f32, f32), dt_ms: u32) {
        let dt = dt_ms.min(100) as f32 / 1000.0;
        self.vel.0 = (self.vel.0 + tilt.0 * GRAVITY * dt) * DAMPING;
        self.vel.1 = (self.vel.1 + tilt.1 * GRAVITY * dt) * DAMPING;
        // Sub-step so a fast ball can't skip through a 1 px wall.
        let steps = ((self.vel.0.abs().max(self.vel.1.abs()) * dt) / 0.4) as u32 + 1;
        for _ in 0..steps.min(20) {
            let nx = self.ball.0 + self.vel.0 * dt / steps as f32;
            if self.wall(nx, self.ball.1) {
                self.vel.0 *= -0.3;
            } else {
                self.ball.0 = nx;
            }
            let ny = self.ball.1 + self.vel.1 * dt / steps as f32;
            if self.wall(self.ball.0, ny) {
                self.vel.1 *= -0.3;
            } else {
                self.ball.1 = ny;
            }
        }
        self.leave_trail();
    }

    /// Demo mode: glide along the shortest path to the goal.
    pub fn autopilot(&mut self, dt_ms: u32) {
        if self.path.is_empty() {
            self.path = self.route();
        }
        let Some(&(cx, cy)) = self.path.first() else { return };
        let target = ((cx * PITCH) as f32 + MID, (cy * PITCH) as f32 + MID);
        let (dx, dy) = (target.0 - self.ball.0, target.1 - self.ball.1);
        let dist = (dx * dx + dy * dy).sqrt_approx();
        let step = 14.0 * dt_ms as f32 / 1000.0;
        if dist <= step {
            self.ball = target;
            self.path.remove(0);
        } else {
            self.ball.0 += dx / dist * step;
            self.ball.1 += dy / dist * step;
        }
        self.vel = (0.0, 0.0);
        self.leave_trail();
    }

    fn leave_trail(&mut self) {
        let p = (self.ball.0 as usize, self.ball.1 as usize);
        if self.trail.back() != Some(&p) {
            self.trail.push_back(p);
            if self.trail.len() > TRAIL {
                self.trail.pop_front();
            }
        }
    }

    /// Breadth-first search over maze cells from the ball to the goal.
    fn route(&self) -> Vec<(usize, usize)> {
        let start = self.cell();
        let mut prev = vec![usize::MAX; self.cols * self.rows];
        let mut queue = VecDeque::from([start]);
        prev[start.1 * self.cols + start.0] = start.1 * self.cols + start.0;
        while let Some((cx, cy)) = queue.pop_front() {
            if (cx, cy) == self.goal() {
                break;
            }
            let center = ((cx * PITCH) as f32 + MID, (cy * PITCH) as f32 + MID);
            for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (cx as i32 + dx, cy as i32 + dy);
                if nx < 0 || ny < 0 || nx as usize >= self.cols || ny as usize >= self.rows {
                    continue;
                }
                let gap = (center.0 + dx as f32 * MID, center.1 + dy as f32 * MID);
                let i = ny as usize * self.cols + nx as usize;
                if !self.wall(gap.0, gap.1) && prev[i] == usize::MAX {
                    prev[i] = cy * self.cols + cx;
                    queue.push_back((nx as usize, ny as usize));
                }
            }
        }
        let mut path = Vec::new();
        let mut i = self.goal().1 * self.cols + self.goal().0;
        while prev[i] != usize::MAX && prev[i] != i {
            path.push((i % self.cols, i / self.cols));
            i = prev[i];
        }
        path.reverse();
        path
    }
}

/// Colors for the maze; the ball is a 2x2 dot and the goal a wedge of cheese.
pub struct MazeView<'a> {
    pub maze: &'a Maze,
    pub wall: Color,
    pub floor: Color,
    pub trail: Color,
    pub ball: Color,
}

const CHEESE: Color = Color::Rgb(255, 205, 60);
const CHEESE_HOLE: Color = Color::Rgb(200, 140, 20);
/// A 4x3 wedge of cheese with holes.
const WEDGE: [[Option<Color>; 4]; 3] = [
    [None, None, None, Some(CHEESE)],
    [None, Some(CHEESE), Some(CHEESE_HOLE), Some(CHEESE)],
    [Some(CHEESE), Some(CHEESE), Some(CHEESE), Some(CHEESE_HOLE)],
];

/// Top-left of the 2x2 dot: around the ball, nudged to sit clear of walls.
fn dot_origin(m: &Maze) -> (usize, usize) {
    let (bx, by) = (m.ball.0 as usize, m.ball.1 as usize);
    let overlap = |x0: usize, y0: usize| {
        [(x0, y0), (x0 + 1, y0), (x0, y0 + 1), (x0 + 1, y0 + 1)].iter().filter(|&&(x, y)| m.wall(x as f32, y as f32)).count()
    };
    [(0, 0), (-1, 0), (0, -1), (-1, -1)]
        .iter()
        .map(|&(dx, dy)| ((bx as i32 + dx).max(0) as usize, (by as i32 + dy).max(0) as usize))
        .min_by_key(|&(x, y)| overlap(x, y))
        .unwrap()
}

fn cheese(m: &Maze, x: usize, y: usize) -> Option<Color> {
    let (gx, gy) = m.goal();
    let (cx, cy) = (gx * PITCH + 1, gy * PITCH + 2);
    if (cx..cx + 4).contains(&x) && (cy..cy + 3).contains(&y) { WEDGE[y - cy][x - cx] } else { None }
}

impl Widget for MazeView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let m = self.maze;
        let (dx, dy) = dot_origin(m);
        let pixel = |x: usize, y: usize| {
            if y >= m.height {
                self.floor
            } else if (dx..dx + 2).contains(&x) && (dy..dy + 2).contains(&y) && !m.walls[y * m.width + x] {
                self.ball
            } else if let Some(color) = cheese(m, x, y) {
                color
            } else if m.walls[y * m.width + x] {
                self.wall
            } else if m.trail.contains(&(x, y)) {
                self.trail
            } else {
                self.floor
            }
        };
        let (w, h) = m.size();
        let area = area.intersection(Rect::new(area.x, area.y, w, h));
        for row in 0..area.height {
            for col in 0..area.width {
                let (x, y) = (col as usize, row as usize * 2);
                buf[(area.x + col, area.y + row)]
                    .set_symbol("▀")
                    .set_fg(pixel(x, y))
                    .set_bg(pixel(x, y + 1));
            }
        }
    }
}

trait SqrtApprox {
    fn sqrt_approx(self) -> f32;
}

impl SqrtApprox for f32 {
    /// Newton's method; `core` has no `f32::sqrt` without std.
    fn sqrt_approx(self) -> f32 {
        if self <= 0.0 {
            return 0.0;
        }
        let mut x = if self > 1.0 { self } else { 1.0 };
        for _ in 0..8 {
            x = 0.5 * (x + self / x);
        }
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cell_is_reachable_and_autopilot_solves() {
        for seed in 1..20 {
            let mut maze = Maze::new(10, 8, seed);
            assert_eq!(maze.route().last(), Some(&maze.goal()), "seed {seed}");
            for _ in 0..5000 {
                maze.autopilot(33);
                assert!(!maze.wall(maze.ball.0, maze.ball.1), "autopilot went through a wall (seed {seed})");
                if maze.solved() {
                    break;
                }
            }
            assert!(maze.solved(), "seed {seed}");
        }
    }

    #[test]
    fn ball_never_enters_walls() {
        let mut maze = Maze::new(10, 8, 7);
        for i in 0..3000 {
            let t = i as f32 * 0.01;
            maze.roll(((t * 1.7) % 2.0 - 1.0, (t * 1.3) % 2.0 - 1.0), 33);
            assert!(!maze.wall(maze.ball.0, maze.ball.1));
        }
    }
}
