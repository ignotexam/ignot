use super::{Generated, Level, robot, stack};
use crate::grade::GradeKind;
use crate::rng::Rng;
use crate::{Result, invalid};

/// Two tasks in a row. The second cannot be started without the answer to the first, so a slip early on
/// carries all the way to the end.
pub(super) fn chain(rng: &mut Rng, level: Level) -> Result<Generated> {
    // Each part is one level easier than the whole.
    let part = Level::try_from(level.get().saturating_sub(1).max(1))?;
    let first = stack::make(rng, part);
    let Some(&top) = first.stack.last() else {
        return invalid("chain: the stack ended empty");
    };
    let size = part.pick(robot::GRID);
    let (Ok(x), Ok(y)) = (u32::try_from(top % i64::from(size)), u32::try_from(top / i64::from(size) % i64::from(size)))
    else {
        return invalid("chain: the top of the stack is negative");
    };
    let second = robot::make(rng, part, (x, y));
    let prompt = [
        "This task has two parts.".to_string(),
        format!("Part 1. A stack machine starts empty. {}\nProgram:\n{}", stack::rules(), first.program.join("\n")),
        "Let T be the number on top of the stack at the end of Part 1.".to_string(),
        format!("Part 2. {}", robot::text(&second, &format!("(T mod {size}, floor(T / {size}) mod {size})"))),
        robot::ASK.to_string(),
    ]
    .join("\n\n");
    Ok(Generated { prompt, answer: robot::run(&second), grade: GradeKind::Tokens })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::robot::tests::solve_from_text;
    use crate::generate::stack::tests::{program_in, run_from_text};

    #[test]
    fn the_key_needs_the_first_answer_to_reach_the_second() {
        for level in Level::ALL {
            for i in 0..20 {
                let question = chain(&mut Rng::from_text(&format!("chain {i}")), level).unwrap();
                let top = *run_from_text(program_in(&question.prompt)).last().unwrap() as usize;
                let size = if level.get() <= 4 { 10 } else { 14 };
                assert!(question.prompt.contains(&format!("(T mod {size}, floor(T / {size}) mod {size})")));
                let part_two = &question.prompt[question.prompt.find("Part 2. ").unwrap()..];
                assert_eq!(
                    question.answer,
                    solve_from_text(part_two, (top % size, top / size % size)),
                    "{}",
                    question.prompt
                );
            }
        }
    }
}
