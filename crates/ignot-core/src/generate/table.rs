use super::{Generated, Level};
use crate::date::{days_from_civil, iso_date, weekday};
use crate::grade::GradeKind;
use crate::rng::Rng;

const ROWS: [usize; 5] = [80, 200, 400, 800, 1500];
const CITIES: [&str; 6] = ["Oslo", "Lima", "Pune", "Kobe", "Fez", "Riga"];
const PRODUCTS: [&str; 6] = ["bolt", "gear", "pump", "valve", "seal", "hinge"];
const PRICES_CENTS: [u32; 7] = [250, 375, 420, 710, 995, 1240, 85];
const STATUSES: [&str; 6] = ["ok", "ok", "ok", "ok", "ok", "returned"];
/// Rows are dated within 61 days from here.
const FIRST_DAY: (i64, u32, u32) = (2027, 3, 1);
/// The question asks about this range of dates, both ends included.
const FROM: (i64, u32, u32) = (2027, 3, 9);
const TO: (i64, u32, u32) = (2027, 4, 22);

/// A row of the table. Rows sort by their fields in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    /// Days since 1 January 1970.
    day: i64,
    city: &'static str,
    product: &'static str,
    units: u32,
    cents: u32,
    status: &'static str,
}

struct Task {
    rows: Vec<Row>,
    products: [&'static str; 2],
    excluded: &'static str,
}

fn day((year, month, day): (i64, u32, u32)) -> i64 {
    days_from_civil(year, month, day)
}

fn make(rng: &mut Rng, level: Level) -> Task {
    let mut rows = Vec::with_capacity(level.pick(ROWS));
    for _ in 0..level.pick(ROWS) {
        rows.push(Row {
            day: day(FIRST_DAY) + i64::from(rng.int(0, 60)),
            city: CITIES[rng.index(CITIES.len())],
            product: PRODUCTS[rng.index(PRODUCTS.len())],
            units: rng.int(1, 40),
            cents: *rng.choice(&PRICES_CENTS),
            status: STATUSES[rng.index(STATUSES.len())],
        });
    }
    rows.sort();
    let pair = rng.sample(&PRODUCTS, 2);
    Task { rows, products: [pair[0], pair[1]], excluded: CITIES[rng.index(CITIES.len())] }
}

/// The total in cents. Whole numbers only, so no rounding can creep into the key.
fn solve(task: &Task) -> u64 {
    task.rows
        .iter()
        .filter(|row| {
            task.products.contains(&row.product)
                && row.city != task.excluded
                && row.status == "ok"
                && (day(FROM)..=day(TO)).contains(&row.day)
                && (1..=5).contains(&weekday(row.day))
        })
        .map(|row| u64::from(row.units) * u64::from(row.cents))
        .sum()
}

/// A price the way a spreadsheet shows it: no trailing zeros (`2.5`, `3.75`, `12.4`, `0.85`).
fn price(cents: u32) -> String {
    match (cents / 100, cents % 100) {
        (whole, 0) => whole.to_string(),
        (whole, part) if part % 10 == 0 => format!("{whole}.{}", part / 10),
        (whole, part) => format!("{whole}.{part:02}"),
    }
}

/// An amount with exactly two decimals.
fn money(cents: u64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

pub(super) fn table(rng: &mut Rng, level: Level) -> Generated {
    let task = make(rng, level);
    let mut csv = String::from("date,city,product,units,unit_price,status");
    for row in &task.rows {
        csv.push_str(&format!(
            "\n{},{},{},{},{},{}",
            iso_date(row.day),
            row.city,
            row.product,
            row.units,
            price(row.cents),
            row.status
        ));
    }
    Generated {
        prompt: format!(
            "{csv}\n\nUsing the table above: what is the total revenue (units x unit_price) of rows with status ok, product {} or {}, any city except {}, dated from {} to {} inclusive, counting only Monday-to-Friday dates? Give the number with two decimals.",
            task.products[0],
            task.products[1],
            task.excluded,
            iso_date(day(FROM)),
            iso_date(day(TO)),
        ),
        answer: money(solve(&task)),
        grade: GradeKind::Num,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The day of the week by Zeller's rule, straight from the calendar date: 0 is Saturday, 1 Sunday, 2 Monday.
    fn zeller(year: i64, month: i64, day: i64) -> i64 {
        let (y, m) = if month < 3 { (year - 1, month + 12) } else { (year, month) };
        (day + 13 * (m + 1) / 5 + y % 100 + y % 100 / 4 + y / 100 / 4 + 5 * (y / 100)) % 7
    }

    /// Answers the question from its own text: reads the table back, reads the filters out of the sentence,
    /// and adds up in hundredths parsed from the printed prices.
    fn solve_from_text(prompt: &str) -> String {
        let (csv, question) = prompt.split_once("\n\n").unwrap();
        let word_after =
            |text: &str| question[question.find(text).unwrap() + text.len()..].split([' ', ',']).next().unwrap();
        let (first, second, excluded) = (word_after("product "), word_after(" or "), word_after("except "));
        let (from, to) = (word_after("dated from "), word_after(" to "));
        let mut total = 0u64;
        for line in csv.lines().skip(1) {
            let fields: Vec<&str> = line.split(',').collect();
            let [date, city, product, units, unit_price, status] = fields[..] else {
                panic!("a row without six fields: {line}")
            };
            let parts: Vec<i64> = date.split('-').map(|part| part.parse().unwrap()).collect();
            let weekend = matches!(zeller(parts[0], parts[1], parts[2]), 0 | 1);
            let (whole, fraction) = unit_price.split_once('.').unwrap_or((unit_price, ""));
            let hundredths = whole.parse::<u64>().unwrap() * 100 + format!("{fraction:0<2}").parse::<u64>().unwrap();
            // Dates in this form sort the same as text and as dates.
            if status == "ok"
                && (product == first || product == second)
                && city != excluded
                && date >= from
                && date <= to
                && !weekend
            {
                total += units.parse::<u64>().unwrap() * hundredths;
            }
        }
        format!("{}.{:02}", total / 100, total % 100)
    }

    #[test]
    fn the_key_is_the_total_the_question_asks_for() {
        for level in Level::ALL {
            for i in 0..12 {
                let question = table(&mut Rng::from_text(&format!("table {i}")), level);
                assert_eq!(question.prompt.lines().next(), Some("date,city,product,units,unit_price,status"));
                assert_eq!(question.prompt.split_once("\n\n").unwrap().0.lines().count(), level.pick(ROWS) + 1);
                assert_eq!(question.answer, solve_from_text(&question.prompt));
                assert!(question.prompt.contains("dated from 2027-03-09 to 2027-04-22 inclusive"));
            }
        }
    }

    #[test]
    fn rows_are_sorted_and_the_two_products_differ() {
        for i in 0..20 {
            let task = make(&mut Rng::from_text(&format!("table rows {i}")), Level::ALL[2]);
            assert!(task.rows.is_sorted());
            assert_ne!(task.products[0], task.products[1]);
            assert!(task.rows.iter().all(|row| (day(FIRST_DAY)..=day(FIRST_DAY) + 60).contains(&row.day)));
        }
    }

    #[test]
    fn every_filter_counts() {
        let row = |date: (i64, u32, u32), city, product, status| Row {
            day: day(date),
            city,
            product,
            units: 2,
            cents: 375,
            status,
        };
        let task = |rows: Vec<Row>| Task { rows, products: ["bolt", "gear"], excluded: "Oslo" };
        // Tuesday 9 March 2027, inside the range.
        assert_eq!(solve(&task(vec![row((2027, 3, 9), "Lima", "bolt", "ok")])), 750);
        assert_eq!(solve(&task(vec![row((2027, 3, 9), "Lima", "gear", "ok")])), 750);
        assert_eq!(solve(&task(vec![row((2027, 3, 9), "Lima", "pump", "ok")])), 0);
        assert_eq!(solve(&task(vec![row((2027, 3, 9), "Oslo", "bolt", "ok")])), 0);
        assert_eq!(solve(&task(vec![row((2027, 3, 9), "Lima", "bolt", "returned")])), 0);
        // The day before the range, and a Saturday and a Sunday inside it.
        assert_eq!(solve(&task(vec![row((2027, 3, 8), "Lima", "bolt", "ok")])), 0);
        assert_eq!(solve(&task(vec![row((2027, 3, 13), "Lima", "bolt", "ok")])), 0);
        assert_eq!(solve(&task(vec![row((2027, 3, 14), "Lima", "bolt", "ok")])), 0);
        // The last day of the range is a Thursday; the day after is outside.
        assert_eq!(solve(&task(vec![row((2027, 4, 22), "Lima", "bolt", "ok")])), 750);
        assert_eq!(solve(&task(vec![row((2027, 4, 23), "Lima", "bolt", "ok")])), 0);
    }

    #[test]
    fn prices_and_totals_print_like_the_question_says() {
        assert_eq!(PRICES_CENTS.map(price), ["2.5", "3.75", "4.2", "7.1", "9.95", "12.4", "0.85"]);
        assert_eq!(price(300), "3");
        assert_eq!(price(5), "0.05");
        assert_eq!(money(0), "0.00");
        assert_eq!(money(5), "0.05");
        assert_eq!(money(123_450), "1234.50");
    }
}
