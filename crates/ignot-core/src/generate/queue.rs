use super::{Generated, Level};
use crate::grade::GradeKind;
use crate::rng::Rng;

/// Customers and tellers, per level.
const SIZE: [(usize, usize); 5] = [(20, 2), (35, 3), (60, 3), (100, 3), (180, 4)];

/// Total minutes waited, given (arrival minute, service minutes) in arrival order.
fn total_wait(customers: &[(u32, u32)], tellers: usize) -> u32 {
    let mut free_at = vec![0u32; tellers];
    let mut wait = 0;
    for &(arrives, service) in customers {
        // The teller free earliest; among equals, the first.
        let Some(teller) = free_at.iter_mut().min_by_key(|minute| **minute) else {
            return 0;
        };
        let start = arrives.max(*teller);
        wait += start - arrives;
        *teller = start + service;
    }
    wait
}

pub(super) fn queue(rng: &mut Rng, level: Level) -> Generated {
    let (count, tellers) = level.pick(SIZE);
    let mut minute = 0;
    let mut customers = Vec::with_capacity(count);
    for _ in 0..count {
        minute += rng.int(0, 4);
        customers.push((minute, rng.int(3, 14)));
    }
    let list: Vec<String> = customers
        .iter()
        .enumerate()
        .map(|(i, (arrives, service))| {
            format!("Customer {}: arrives at minute {arrives}, needs {service} minutes", i + 1)
        })
        .collect();
    Generated {
        prompt: format!(
            "A shop has {tellers} tellers, all free at minute 0. Customers are served strictly in arrival order (same-minute arrivals go in customer-number order). Each customer takes the teller that becomes free earliest (ties: the lowest-numbered teller) and starts at the later of their arrival minute and that teller's free minute, keeping the teller for their whole service time.\n{}\nWhat is the total number of minutes all customers spend waiting before their service starts? Answer with the number only.",
            list.join("\n")
        ),
        answer: total_wait(&customers, tellers).to_string(),
        grade: GradeKind::Num,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays the shop minute by minute. A teller that is free takes the next customer in line; when several
    /// are free at once the lowest-numbered one goes first, which is the rule in the question.
    fn minute_by_minute(customers: &[(u32, u32)], tellers: usize) -> u32 {
        let mut busy_until = vec![0u32; tellers];
        let (mut next, mut wait, mut minute) = (0, 0, 0);
        while next < customers.len() {
            let mut order: Vec<usize> = (0..tellers).collect();
            order.sort_by_key(|&teller| (busy_until[teller], teller));
            for teller in order {
                if next < customers.len() && customers[next].0 <= minute && busy_until[teller] <= minute {
                    wait += minute - customers[next].0;
                    busy_until[teller] = minute + customers[next].1;
                    next += 1;
                }
            }
            minute += 1;
        }
        wait
    }

    fn parse(prompt: &str) -> (Vec<(u32, u32)>, usize) {
        let number = |text: &str, after: &str| -> u32 {
            let rest = &text[text.find(after).unwrap() + after.len()..];
            rest[..rest.find(|c: char| !c.is_ascii_digit()).unwrap()].parse().unwrap()
        };
        let customers = prompt
            .lines()
            .filter(|line| line.starts_with("Customer "))
            .map(|line| (number(line, "minute "), number(line, "needs ")))
            .collect();
        (customers, number(prompt, "A shop has ") as usize)
    }

    #[test]
    fn worked_examples() {
        // One teller: the second customer waits for the first to finish.
        assert_eq!(total_wait(&[(0, 5), (1, 3)], 1), 4);
        // Two tellers: nobody waits.
        assert_eq!(total_wait(&[(0, 5), (1, 3)], 2), 0);
        // Two tellers, three at once: the third waits for the shorter service (3 minutes).
        assert_eq!(total_wait(&[(0, 5), (0, 3), (0, 4)], 2), 3);
        // A tie goes to the lowest-numbered teller, which here is busy for longer afterwards.
        assert_eq!(total_wait(&[(0, 4), (0, 4), (4, 10), (4, 1), (5, 1)], 2), 0);
        assert_eq!(total_wait(&[], 3), 0);
    }

    #[test]
    fn the_key_is_the_wait_in_the_question() {
        for level in Level::ALL {
            for i in 0..20 {
                let question = queue(&mut Rng::from_text(&format!("queue {i}")), level);
                let (customers, tellers) = parse(&question.prompt);
                assert_eq!((customers.len(), tellers), level.pick(SIZE));
                assert!(customers.windows(2).all(|pair| pair[0].0 <= pair[1].0), "arrivals are in order");
                assert_eq!(question.answer, minute_by_minute(&customers, tellers).to_string());
            }
        }
    }
}
