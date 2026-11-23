use std::collections::BTreeSet;

use super::{Generated, Level};
use crate::grade::GradeKind;
use crate::rng::Rng;

const COMMANDS: [usize; 5] = [15, 35, 70, 140, 280];
/// Side of the grid, per level.
pub(super) const GRID: [u32; 5] = [10, 10, 10, 14, 18];
/// Headings in clockwise order. North increases y and east increases x.
const HEADINGS: [char; 4] = ['N', 'E', 'S', 'W'];

pub(super) const ASK: &str = "Where does it end and which way does it face? Answer as x,y,D where D is N, E, S or W.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Forward(u32),
    Left,
    Right,
    Back,
}

pub(super) struct Task {
    pub(super) size: u32,
    start: (u32, u32),
    blocked: BTreeSet<(u32, u32)>,
    commands: Vec<Command>,
}

/// A grid with walls and a program. The start cell and the eight cells around it are never blocked, so the
/// robot can always leave.
pub(super) fn make(rng: &mut Rng, level: Level, start: (u32, u32)) -> Task {
    let size = level.pick(GRID);
    let mut blocked = BTreeSet::new();
    while blocked.len() < (size * size / 7) as usize {
        let x = rng.int(0, size - 1);
        let y = rng.int(0, size - 1);
        if x.abs_diff(start.0) > 1 || y.abs_diff(start.1) > 1 {
            blocked.insert((x, y));
        }
    }
    let mut commands = Vec::with_capacity(level.pick(COMMANDS));
    for _ in 0..level.pick(COMMANDS) {
        commands.push(match *rng.choice(&['F', 'F', 'L', 'R', 'B']) {
            'F' => Command::Forward(rng.int(1, 6)),
            'L' => Command::Left,
            'R' => Command::Right,
            _ => Command::Back,
        });
    }
    Task { size, start, blocked, commands }
}

/// Where the robot ends, as `x,y,D`.
pub(super) fn run(task: &Task) -> String {
    let (mut x, mut y) = task.start;
    let mut heading = 0;
    for command in &task.commands {
        match *command {
            Command::Forward(cells) => {
                for _ in 0..cells {
                    // A step off the low edge wraps to a huge number, which is off the grid like any other.
                    let next = match heading {
                        0 => (x, y.wrapping_add(1)),
                        1 => (x.wrapping_add(1), y),
                        2 => (x, y.wrapping_sub(1)),
                        _ => (x.wrapping_sub(1), y),
                    };
                    if next.0 >= task.size || next.1 >= task.size || task.blocked.contains(&next) {
                        break;
                    }
                    (x, y) = next;
                }
            }
            Command::Left => heading = (heading + 3) % 4,
            Command::Right => heading = (heading + 1) % 4,
            Command::Back => heading = (heading + 2) % 4,
        }
    }
    format!("{x},{y},{}", HEADINGS[heading])
}

/// The task in words. `start` is how the start cell is written, which the two-part kind replaces by a formula.
pub(super) fn text(task: &Task, start: &str) -> String {
    let cells: Vec<String> = task.blocked.iter().map(|(x, y)| format!("({x},{y})")).collect();
    let program: Vec<String> = task
        .commands
        .iter()
        .map(|command| match command {
            Command::Forward(cells) => format!("F{cells}"),
            Command::Left => "L".into(),
            Command::Right => "R".into(),
            Command::Back => "B".into(),
        })
        .collect();
    format!(
        "A robot is on a {size}x{size} grid of cells (x, y), with x and y from 0 to {last}. It starts at {start} facing north (north increases y; east increases x). Blocked cells: {}.\nCommands: Fk moves forward k cells one at a time, stopping early (and skipping the rest of that command) if the next cell is off the grid or blocked; L and R turn 90 degrees left or right; B turns around.\nProgram: {}",
        cells.join(", "),
        program.join(" "),
        size = task.size,
        last = task.size - 1,
    )
}

pub(super) fn robot(rng: &mut Rng, level: Level) -> Generated {
    let task = make(rng, level, (0, 0));
    Generated { prompt: format!("{}\n{ASK}", text(&task, "(0,0)")), answer: run(&task), grade: GradeKind::Tokens }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// Solves the task from its own text, with a different method: the grid as a picture, the heading as a
    /// compass word, and whole moves looked up cell by cell.
    pub(in crate::generate) fn solve_from_text(prompt: &str, start: (usize, usize)) -> String {
        let between = |from: &str, to: &str| {
            let rest = &prompt[prompt.find(from).unwrap() + from.len()..];
            &rest[..rest.find(to).unwrap()]
        };
        let size: usize = between("is on a ", "x").parse().unwrap();
        let mut wall = vec![vec![false; size]; size];
        for cell in between("Blocked cells: ", ".\n").split(", ") {
            let (x, y) = cell.trim_matches(['(', ')']).split_once(',').unwrap();
            wall[y.parse::<usize>().unwrap()][x.parse::<usize>().unwrap()] = true;
        }
        let program = prompt[prompt.find("Program: ").unwrap() + 9..].lines().next().unwrap();
        let (mut x, mut y) = start;
        let mut facing = "north";
        for command in program.split(' ') {
            facing = match (command, facing) {
                ("L", "north") | ("R", "south") | ("B", "east") => "west",
                ("L", "west") | ("R", "east") | ("B", "north") => "south",
                ("L", "south") | ("R", "north") | ("B", "west") => "east",
                ("L", "east") | ("R", "west") | ("B", "south") => "north",
                _ => facing,
            };
            if let Some(cells) = command.strip_prefix('F') {
                for _ in 0..cells.parse::<usize>().unwrap() {
                    let next = match facing {
                        "north" if y + 1 < size => (x, y + 1),
                        "east" if x + 1 < size => (x + 1, y),
                        "south" if y > 0 => (x, y - 1),
                        "west" if x > 0 => (x - 1, y),
                        _ => break,
                    };
                    if wall[next.1][next.0] {
                        break;
                    }
                    (x, y) = next;
                }
            }
        }
        format!("{x},{y},{}", facing[..1].to_uppercase())
    }

    fn task(size: u32, blocked: &[(u32, u32)], commands: &[Command]) -> Task {
        Task { size, start: (0, 0), blocked: blocked.iter().copied().collect(), commands: commands.to_vec() }
    }

    #[test]
    fn worked_examples() {
        use Command::{Back, Forward, Left, Right};
        // Up three, right two.
        assert_eq!(run(&task(5, &[], &[Forward(3), Right, Forward(2)])), "2,3,E");
        // Stops at the edge of the grid and skips the rest of that move.
        assert_eq!(run(&task(5, &[], &[Forward(6), Forward(6), Right, Forward(1)])), "1,4,E");
        // Stops in front of a wall.
        assert_eq!(run(&task(5, &[(0, 2)], &[Forward(4), Right, Forward(1), Left, Forward(4)])), "1,4,N");
        // Turning: left of north is west, and around is south.
        assert_eq!(run(&task(5, &[], &[Left])), "0,0,W");
        assert_eq!(run(&task(5, &[], &[Back, Forward(2)])), "0,0,S");
        assert_eq!(run(&task(5, &[], &[Left, Left, Left, Left, Right, Right, Back])), "0,0,N");
    }

    #[test]
    fn the_key_is_where_the_robot_in_the_question_ends() {
        for level in Level::ALL {
            for i in 0..20 {
                let question = robot(&mut Rng::from_text(&format!("robot {i}")), level);
                assert_eq!(question.answer, solve_from_text(&question.prompt, (0, 0)), "{}", question.prompt);
            }
        }
    }

    #[test]
    fn the_start_and_the_cells_around_it_are_free() {
        for level in Level::ALL {
            for start in [(0, 0), (5, 5), (9, 0), (3, 9)] {
                let task = make(&mut Rng::from_text("robot start"), level, start);
                assert_eq!(task.blocked.len(), (task.size * task.size / 7) as usize);
                assert!(task.blocked.iter().all(|&(x, y)| x < task.size
                    && y < task.size
                    && (x.abs_diff(start.0) > 1 || y.abs_diff(start.1) > 1)));
                assert_eq!(task.commands.len(), level.pick(COMMANDS));
            }
        }
    }
}
