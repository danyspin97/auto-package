use super::*;
use ap_build::pipeline::CandidateSource::Search;
use std::cell::RefCell;

const TTY: Terminal = Terminal {
    stdin: true,
    stderr: true,
    tui: false,
};

fn list(n: usize) -> Vec<Candidate> {
    (1..=n)
        .map(|i| Candidate::new(&format!("https://github.com/o{i}/pleamar"), Search))
        .collect()
}

/// Run `pick` with scripted answers; returns the pick, the prompts asked and
/// what was shown.
fn run(
    cands: &[Candidate],
    term: Terminal,
    answers: &[Option<&str>],
) -> (Option<String>, Vec<String>, Vec<String>) {
    let left = RefCell::new(answers.iter().map(|a| a.map(str::to_string)));
    let asked = RefCell::new(Vec::new());
    let shown = RefCell::new(Vec::new());
    let got = pick(
        "pleamar",
        cands,
        term,
        (100, false),
        &|s| shown.borrow_mut().push(s.to_string()),
        &|p| {
            asked.borrow_mut().push(p.to_string());
            left.borrow_mut().next().flatten()
        },
    )
    .map(|c| c.url.clone());
    (got, asked.into_inner(), shown.into_inner())
}

#[test]
fn a_valid_number_picks_that_candidate() {
    let (got, asked, shown) = run(&list(1), TTY, &[Some("1")]);
    assert_eq!(got.as_deref(), Some("https://github.com/o1/pleamar"));
    assert_eq!(asked, ["Build which? [1, Enter to cancel] "]);
    assert!(shown[0].contains("  1. github.com/o1/pleamar"));
}

#[test]
fn two_of_three_is_the_second() {
    let (got, asked, _) = run(&list(3), TTY, &[Some(" 2 ")]);
    assert_eq!(got.as_deref(), Some("https://github.com/o2/pleamar"));
    assert_eq!(asked, ["Build which? [1-3, Enter to cancel] "]);
}

#[test]
fn an_out_of_range_answer_is_asked_again() {
    let (got, asked, shown) = run(&list(3), TTY, &[Some("4"), Some("0"), Some("3")]);
    assert_eq!(got.as_deref(), Some("https://github.com/o3/pleamar"));
    assert_eq!(asked.len(), 3);
    assert!(shown[1].contains("not a number from 1-3: 4"));
}

#[test]
fn enter_and_end_of_input_cancel() {
    assert_eq!(run(&list(2), TTY, &[Some("")]).0, None);
    assert_eq!(run(&list(2), TTY, &[Some("  ")]).1.len(), 1);
    assert_eq!(run(&list(2), TTY, &[None]).0, None);
    assert_eq!(run(&list(2), TTY, &[Some("x"), None]).0, None);
}

#[test]
fn n_no_q_and_quit_cancel_at_once_in_any_case() {
    for word in ["n", "no", "q", "quit", " N ", "No", "QUIT"] {
        let (got, asked, _) = run(&list(2), TTY, &[Some(word), Some("1")]);
        assert_eq!((got, asked.len()), (None, 1), "answer {word:?}");
    }
}

#[test]
fn a_leading_plus_is_not_a_number() {
    let (got, asked, shown) = run(&list(3), TTY, &[Some("+2"), Some("2")]);
    assert_eq!(got.as_deref(), Some("https://github.com/o2/pleamar"));
    assert_eq!(asked.len(), 2);
    assert!(shown[1].contains("not a number from 1-3: +2"));
}

#[test]
fn three_invalid_answers_cancel() {
    let (got, asked, _) = run(
        &list(2),
        TTY,
        &[Some("a"), Some("9"), Some("-1"), Some("1")],
    );
    assert_eq!(got, None);
    assert_eq!(asked.len(), 3);
}

#[test]
fn without_a_terminal_nothing_is_shown_or_read() {
    for term in [
        Terminal {
            stdin: false,
            ..TTY
        },
        Terminal {
            stderr: false,
            ..TTY
        },
        Terminal { tui: true, ..TTY },
    ] {
        let (got, asked, shown) = run(&list(2), term, &[Some("1")]);
        assert_eq!((got, asked, shown), (None, vec![], vec![]));
    }
    assert_eq!(run(&[], TTY, &[Some("1")]), (None, vec![], vec![]));
}
