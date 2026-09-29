//! A patch between two versions of a file, written here rather than asked
//! of git: what a session changed is measured from the copies rewind keeps,
//! which works in a folder git knows nothing about.
//!
//! The lines are compared with Myers' algorithm after the lines both
//! versions start and end with are set aside. A change too tangled to
//! compare within [`MOST_EDITS`] is shown as every old line going and every
//! new one coming, which is still true, only longer.

/// Unchanged lines shown either side of a change, as git shows them.
const CONTEXT: usize = 3;

/// Edits the comparison looks for before giving up. Its memory grows with
/// the square of this, so it stays under ten megabytes.
const MOST_EDITS: usize = 1000;

/// A patch, with how many lines it adds and removes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Patch {
    pub text: String,
    pub added: usize,
    pub removed: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Same,
    Gone,
    New,
}

/// The patch that turns `old` into `new`, headed with `old_name` and
/// `new_name` as git heads one (`/dev/null` for a side that is not there).
pub(crate) fn patch(old: &str, new: &str, old_name: &str, new_name: &str) -> Patch {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let ops = compare(&a, &b);
    let added = ops.iter().filter(|op| **op == Op::New).count();
    let removed = ops.iter().filter(|op| **op == Op::Gone).count();
    if added == 0 && removed == 0 {
        return Patch::default();
    }
    let mut text = format!("--- {old_name}\n+++ {new_name}\n");
    for hunk in hunks(&ops) {
        write_hunk(
            &mut text,
            &ops[hunk.clone()],
            &a,
            &b,
            start_of(&ops, hunk.start),
        );
    }
    Patch {
        text,
        added,
        removed,
    }
}

/// The ops that turn `a` into `b`, in order.
fn compare(a: &[&str], b: &[&str]) -> Vec<Op> {
    let head = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (mid_a, mid_b) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let middle = myers(mid_a, mid_b).unwrap_or_else(|| {
        let gone = std::iter::repeat_n(Op::Gone, mid_a.len());
        gone.chain(std::iter::repeat_n(Op::New, mid_b.len()))
            .collect()
    });
    let mut ops = vec![Op::Same; head];
    ops.extend(middle);
    ops.extend(std::iter::repeat_n(Op::Same, tail));
    ops
}

/// The shortest edit from `a` to `b`, or `None` past [`MOST_EDITS`].
fn myers(a: &[&str], b: &[&str]) -> Option<Vec<Op>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let most = (a.len() + b.len()).min(MOST_EDITS) as isize;
    // v[k] is the furthest x on diagonal k; each round keeps its own copy
    // of the diagonals it could reach, -d-1..=d+1, to walk back through.
    let at = |d: isize, k: isize| (k + d + 1) as usize;
    let mut rounds: Vec<Vec<isize>> = Vec::new();
    for d in 0..=most {
        let mut next = vec![0isize; (2 * d + 3) as usize];
        let before = |k: isize| match rounds.last() {
            Some(v) if (-d..=d).contains(&k) => v[at(d - 1, k)],
            _ => 0,
        };
        let mut done = false;
        for k in (-d..=d).step_by(2) {
            let mut x = if k == -d || (k != d && before(k - 1) < before(k + 1)) {
                before(k + 1)
            } else {
                before(k - 1) + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            next[at(d, k)] = x;
            if x >= n && y >= m {
                done = true;
                break;
            }
        }
        rounds.push(next);
        if done {
            return Some(walk_back(&rounds, n, m));
        }
    }
    None
}

/// The ops of the path the rounds found, walked back from the end.
fn walk_back(rounds: &[Vec<isize>], n: isize, m: isize) -> Vec<Op> {
    let at = |d: isize, k: isize| (k + d + 1) as usize;
    let (mut x, mut y) = (n, m);
    let mut ops = Vec::new();
    for d in (1..rounds.len() as isize).rev() {
        let prev = &rounds[(d - 1) as usize];
        let reach = |k: isize| {
            if k < -(d - 1) || k > d - 1 {
                0
            } else {
                prev[at(d - 1, k)]
            }
        };
        let k = x - y;
        let prev_k = if k == -d || (k != d && reach(k - 1) < reach(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = reach(prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops.push(Op::Same);
            x -= 1;
            y -= 1;
        }
        ops.push(if x == prev_x { Op::New } else { Op::Gone });
        (x, y) = (prev_x, prev_y);
    }
    ops.extend(std::iter::repeat_n(Op::Same, x as usize));
    ops.reverse();
    ops
}

/// The stretches of `ops` a patch shows: each change with its context,
/// changes whose context meets joined into one.
fn hunks(ops: &[Op]) -> Vec<std::ops::Range<usize>> {
    let mut hunks: Vec<std::ops::Range<usize>> = Vec::new();
    for (at, op) in ops.iter().enumerate() {
        if *op == Op::Same {
            continue;
        }
        let start = at.saturating_sub(CONTEXT);
        let end = (at + 1 + CONTEXT).min(ops.len());
        match hunks.last_mut() {
            Some(last) if start <= last.end => last.end = end,
            _ => hunks.push(start..end),
        }
    }
    hunks
}

/// The line numbers in `a` and `b`, from 0, where op `upto` starts.
fn start_of(ops: &[Op], upto: usize) -> (usize, usize) {
    ops[..upto].iter().fold((0, 0), |(a, b), op| match op {
        Op::Same => (a + 1, b + 1),
        Op::Gone => (a + 1, b),
        Op::New => (a, b + 1),
    })
}

fn write_hunk(text: &mut String, ops: &[Op], a: &[&str], b: &[&str], start: (usize, usize)) {
    let olds = ops.iter().filter(|op| **op != Op::New).count();
    let news = ops.iter().filter(|op| **op != Op::Gone).count();
    // A side with no lines is numbered from the line before it, as git does.
    let first = |at: usize, count: usize| if count == 0 { at } else { at + 1 };
    text.push_str(&format!(
        "@@ -{},{olds} +{},{news} @@\n",
        first(start.0, olds),
        first(start.1, news)
    ));
    let (mut x, mut y) = start;
    for op in ops {
        let (mark, line) = match op {
            Op::Same => (' ', a[x]),
            Op::Gone => ('-', a[x]),
            Op::New => ('+', b[y]),
        };
        text.push(mark);
        text.push_str(line);
        text.push('\n');
        match op {
            Op::Same => (x, y) = (x + 1, y + 1),
            Op::Gone => x += 1,
            Op::New => y += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_changed_line_is_one_hunk_with_its_context() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n";
        let new = "1\n2\n3\n4\nfive\n6\n7\n8\n";
        let patch = patch(old, new, "a/f", "b/f");
        assert_eq!(
            patch.text,
            "--- a/f\n+++ b/f\n@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+five\n 6\n 7\n 8\n"
        );
        assert_eq!((patch.added, patch.removed), (1, 1));
    }

    #[test]
    fn far_apart_changes_are_separate_hunks() {
        let old: String = (0..20).map(|n| format!("{n}\n")).collect();
        let new = old.replace("2\n", "two\n").replace("17\n", "seventeen\n");
        let patch = patch(&old, &new, "a", "b");
        assert_eq!(patch.text.matches("@@ -").count(), 2, "{}", patch.text);
        assert!(patch.text.contains("-17\n+seventeen\n"), "{}", patch.text);
    }

    #[test]
    fn a_new_file_is_all_added_and_numbered_from_nothing() {
        let patch = patch("", "a\nb\n", "/dev/null", "b/f");
        assert_eq!(
            patch.text,
            "--- /dev/null\n+++ b/f\n@@ -0,0 +1,2 @@\n+a\n+b\n"
        );
        assert_eq!((patch.added, patch.removed), (2, 0));
    }

    #[test]
    fn the_same_text_is_no_patch() {
        assert_eq!(patch("a\n", "a\n", "a", "b"), Patch::default());
    }

    #[test]
    fn inserted_and_removed_lines_are_found_among_the_same_ones() {
        let ops = compare(&["a", "b", "c", "d"], &["a", "x", "c", "d", "e"]);
        assert_eq!(
            ops,
            vec![Op::Same, Op::Gone, Op::New, Op::Same, Op::Same, Op::New]
        );
    }

    #[test]
    fn a_change_too_tangled_to_compare_is_a_replacement() {
        let old: Vec<String> = (0..3000).map(|n| format!("a{n}")).collect();
        let new: Vec<String> = (0..3000).map(|n| format!("b{n}")).collect();
        let old: Vec<&str> = old.iter().map(String::as_str).collect();
        let new: Vec<&str> = new.iter().map(String::as_str).collect();
        let ops = compare(&old, &new);
        assert_eq!(ops.iter().filter(|op| **op == Op::Gone).count(), 3000);
        assert_eq!(ops.iter().filter(|op| **op == Op::New).count(), 3000);
    }
}
