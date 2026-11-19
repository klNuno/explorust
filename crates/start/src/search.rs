//! Search as you type, the pure part: text folding, the ranking of apps and settings (prefix, then
//! word prefix, keyword, substring, then fuzzy, with a bonus for apps used often), the calculator,
//! and the rows of the results view (best match, then groups by kind).

/// What a result is, in the order Windows groups them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Calc,
    App,
    Setting,
    Folder,
    File,
}

/// Lowercase, accents folded, so `parametres` finds `Paramètres`.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        for l in c.to_lowercase() {
            out.push(match l {
                'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
                'ç' => 'c',
                'è' | 'é' | 'ê' | 'ë' => 'e',
                'ì' | 'í' | 'î' | 'ï' => 'i',
                'ñ' => 'n',
                'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
                'ù' | 'ú' | 'û' | 'ü' => 'u',
                'ý' | 'ÿ' => 'y',
                '’' => '\'',
                c => c,
            });
        }
    }
    out
}

fn is_sep(c: char) -> bool {
    matches!(c, ' ' | '-' | '.' | '_' | '(' | ')' | '/' | '\\' | ',' | '&' | ':' | '\'')
}

/// A searchable entry with its text folded once, when the source is loaded.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub keywords: Vec<String>,
}

impl Entry {
    pub fn new(name: &str, keywords: &[String]) -> Self {
        Entry { name: fold(name), keywords: keywords.iter().map(|k| fold(k)).filter(|k| !k.is_empty()).collect() }
    }
}

/// Fuzzy match of `q` as a subsequence of `s`: a score from 0 to 200, higher when the letters
/// fall on word starts and close together; None when `q` is not a subsequence.
fn fuzzy(s: &str, q: &str) -> Option<u32> {
    let s: Vec<char> = s.chars().collect();
    let mut score: i32 = 200;
    // The first letter must start a word, as initials do: "calc" does not find "Local Security".
    let q0 = q.chars().find(|c| *c != ' ')?;
    let mut i = (0..s.len()).find(|&i| s[i] == q0 && (i == 0 || is_sep(s[i - 1])))?;
    let mut last: Option<usize> = None;
    for qc in q.chars().filter(|c| *c != ' ') {
        let mut found = None;
        while i < s.len() {
            if s[i] == qc {
                found = Some(i);
                break;
            }
            i += 1;
        }
        let at = found?;
        let word_start = at == 0 || is_sep(s[at - 1]);
        if !word_start {
            score -= 6;
        }
        if let Some(l) = last {
            score -= ((at - l - 1) as i32).min(8) * 2;
        }
        last = Some(at);
        i = at + 1;
    }
    Some(score.max(1) as u32)
}

/// The score of an entry for a folded, trimmed query; None when it does not match.
pub fn score(e: &Entry, q: &str) -> Option<u32> {
    if q.is_empty() {
        return None;
    }
    let n = &e.name;
    let word_prefix = |s: &str| s.split(is_sep).any(|w| w.starts_with(q));
    let s = if n == q {
        1100
    } else if n.starts_with(q) {
        1000
    } else if word_prefix(n) {
        850
    } else if e.keywords.iter().any(|k| k.starts_with(q) || word_prefix(k)) {
        650
    } else if n.contains(q) {
        500
    } else if q.chars().count() >= 2 {
        // Initials and typos come last: "wt" for Windows Terminal, "calcu" with a missing letter.
        fuzzy(n, q).filter(|&f| f >= 150)?
    } else {
        return None;
    };
    // Shorter names first among equals: "Paint" before "Paint 3D".
    Some(s * 4 + (60 - n.chars().count().min(60) as u32) / 4)
}

/// Bonus for an app opened `uses` times: up to the gap between two match tiers.
pub fn use_bonus(uses: u32) -> u32 {
    (uses * 40).min(600)
}

/// The best `max` entries for a query, as (index, score), best first.
pub fn rank(entries: &[Entry], bonus: impl Fn(usize) -> u32, query: &str, max: usize) -> Vec<(usize, u32)> {
    let q = fold(query.trim());
    let mut out: Vec<(usize, u32)> =
        entries.iter().enumerate().filter_map(|(i, e)| score(e, &q).map(|s| (i, s + bonus(i)))).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out.truncate(max);
    out
}

/// A simple expression: `+ - * / ^ %`, `x` and `×` for times, `÷`, parentheses, unary minus and
/// decimals with a dot or a comma. None unless it has an operator and evaluates to a finite number.
pub fn calc(input: &str) -> Option<f64> {
    let s: Vec<char> = input
        .trim()
        .trim_start_matches('=')
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            'x' | 'X' | '×' => '*',
            '÷' => '/',
            ',' => '.',
            c => c,
        })
        .collect();
    if s.is_empty() || !s.iter().any(|c| c.is_ascii_digit()) || !s.iter().any(|c| "+-*/^%(".contains(*c)) {
        return None;
    }
    if !s.iter().all(|c| c.is_ascii_digit() || "+-*/^%().".contains(*c)) {
        return None;
    }
    let mut p = Parser { s: &s, i: 0 };
    let v = p.expr()?;
    (p.i == s.len() && v.is_finite()).then_some(v)
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }
    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(c @ ('+' | '-')) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if c == '+' { v + r } else { v - r };
        }
        Some(v)
    }
    fn term(&mut self) -> Option<f64> {
        let mut v = self.power()?;
        loop {
            match self.peek() {
                Some(c @ ('*' | '/')) => {
                    self.i += 1;
                    let r = self.power()?;
                    v = if c == '*' { v * r } else { v / r };
                }
                Some('%') => {
                    self.i += 1;
                    v /= 100.0;
                }
                _ => return Some(v),
            }
        }
    }
    fn power(&mut self) -> Option<f64> {
        let b = self.unary()?;
        if self.peek() == Some('^') {
            self.i += 1;
            let e = self.power()?;
            return Some(b.powf(e));
        }
        Some(b)
    }
    fn unary(&mut self) -> Option<f64> {
        match self.peek()? {
            '-' => {
                self.i += 1;
                Some(-self.unary()?)
            }
            '+' => {
                self.i += 1;
                self.unary()
            }
            '(' => {
                self.i += 1;
                let v = self.expr()?;
                if self.peek() == Some(')') {
                    self.i += 1;
                }
                Some(v)
            }
            _ => self.number(),
        }
    }
    fn number(&mut self) -> Option<f64> {
        let start = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.') {
            self.i += 1;
        }
        let t: String = self.s[start..self.i].iter().collect();
        t.parse().ok()
    }
}

/// A calculator result as Windows prints it: up to 12 significant digits, no trailing zeros, a
/// decimal comma in French.
pub fn format_number(v: f64, french: bool) -> String {
    let v = if v == 0.0 { 0.0 } else { v };
    let mut s = if v.abs() >= 1e15 || (v != 0.0 && v.abs() < 1e-9) {
        format!("{v:e}")
    } else {
        let digits = (12 - (v.abs().log10().floor() as i32 + 1)).clamp(0, 12) as usize;
        let mut s = format!("{v:.digits$}");
        if s.contains('.') {
            s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        }
        s
    };
    if french {
        s = s.replace('.', ",");
    }
    s
}

/// One hit in the results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub kind: Kind,
    /// Index in the source of its kind (apps, settings, files of this query; 0 for the calculator).
    pub index: usize,
    pub score: u32,
}

/// A line of the results list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    BestHeader,
    Header(Kind),
    Item(Hit),
}

/// How many items each group shows under the best match.
pub const GROUP_MAX: usize = 5;

/// The best match and the groups below it, as rows. The calculator wins when present; otherwise the
/// best scored app or setting; files only when nothing else matches. Groups follow Windows' order.
pub fn rows(calc: bool, apps: &[(usize, u32)], settings: &[(usize, u32)], folders: &[usize], files: &[usize]) -> Vec<Row> {
    let mut best;
    if calc {
        best = Some(Hit { kind: Kind::Calc, index: 0, score: u32::MAX });
    } else {
        let a = apps.first().map(|&(i, s)| Hit { kind: Kind::App, index: i, score: s });
        let st = settings.first().map(|&(i, s)| Hit { kind: Kind::Setting, index: i, score: s });
        best = match (a, st) {
            (Some(a), Some(s)) => Some(if s.score > a.score { s } else { a }),
            (a, s) => a.or(s),
        };
        if best.is_none() {
            best = folders.first().map(|&i| Hit { kind: Kind::Folder, index: i, score: 0 }).or(files.first().map(|&i| Hit { kind: Kind::File, index: i, score: 0 }));
        }
    }
    let mut out = Vec::new();
    if let Some(b) = best {
        out.push(Row::BestHeader);
        out.push(Row::Item(b));
    }
    let mut group = |kind: Kind, hits: Vec<Hit>| {
        let items: Vec<Hit> = hits.into_iter().filter(|h| Some(*h) != best).take(GROUP_MAX).collect();
        if !items.is_empty() {
            out.push(Row::Header(kind));
            out.extend(items.into_iter().map(Row::Item));
        }
    };
    group(Kind::App, apps.iter().map(|&(i, s)| Hit { kind: Kind::App, index: i, score: s }).collect());
    group(Kind::Setting, settings.iter().map(|&(i, s)| Hit { kind: Kind::Setting, index: i, score: s }).collect());
    group(Kind::Folder, folders.iter().map(|&i| Hit { kind: Kind::Folder, index: i, score: 0 }).collect());
    group(Kind::File, files.iter().map(|&i| Hit { kind: Kind::File, index: i, score: 0 }).collect());
    out
}

/// The next selectable row from `from` in direction `dir` (+1 or -1), skipping headers.
pub fn next_item(rows: &[Row], from: Option<usize>, dir: isize) -> Option<usize> {
    let mut i = match from {
        Some(f) => f as isize + dir,
        None if dir > 0 => 0,
        None => rows.len() as isize - 1,
    };
    while i >= 0 && (i as usize) < rows.len() {
        if matches!(rows[i as usize], Row::Item(_)) {
            return Some(i as usize);
        }
        i += dir;
    }
    from
}

/// Usage counts of apps, `%LOCALAPPDATA%\explorust\start-usage.tsv`: `count<TAB>aumid` per line.
pub fn usage_from_tsv(text: &str) -> Vec<(String, u32)> {
    text.lines()
        .filter_map(|l| {
            let (n, id) = l.split_once('\t')?;
            Some((id.trim().to_string(), n.trim().parse().ok()?))
        })
        .filter(|(id, _)| !id.is_empty())
        .collect()
}

pub fn usage_to_tsv(usage: &[(String, u32)]) -> String {
    usage.iter().map(|(id, n)| format!("{n}\t{id}\r\n")).collect()
}

/// The SQL restriction for the Windows Search index: every word of the query must start a word of
/// the file name (`System.ItemNameDisplay`), as Windows' own search box does. Quotes are doubled.
pub fn index_sql(query: &str, max: u32) -> Option<String> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| w.chars().filter(|c| !matches!(c, '"' | '\'' | '*' | '%' | '[' | ']')).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return None;
    }
    let conds: Vec<String> = words.iter().map(|w| format!("CONTAINS(System.ItemNameDisplay, '\"{w}*\"')")).collect();
    Some(format!(
        "SELECT TOP {max} System.ItemPathDisplay, System.ItemNameDisplay, System.ItemType, System.DateModified FROM SystemIndex \
         WHERE SCOPE='file:' AND {} ORDER BY System.Search.Rank DESC, System.DateModified DESC",
        conds.join(" AND ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(names: &[&str]) -> Vec<Entry> {
        names.iter().map(|n| Entry::new(n, &[])).collect()
    }

    #[test]
    fn folds_accents_and_case() {
        assert_eq!(fold("Paramètres d’Écran"), "parametres d'ecran");
    }

    #[test]
    fn ranks_prefix_then_word_then_fuzzy() {
        let e = entries(&["Windows Terminal", "Terminal Preview", "Paint", "Notepad", "Calculator"]);
        let r: Vec<usize> = rank(&e, |_| 0, "term", 10).into_iter().map(|x| x.0).collect();
        assert_eq!(r, vec![1, 0]);
        let r: Vec<usize> = rank(&e, |_| 0, "wt", 10).into_iter().map(|x| x.0).collect();
        assert_eq!(r, vec![0]);
        let r: Vec<usize> = rank(&e, |_| 0, "calcu", 10).into_iter().map(|x| x.0).collect();
        assert_eq!(r, vec![4]);
        let r: Vec<usize> = rank(&e, |_| 0, "clcultor", 10).into_iter().map(|x| x.0).collect();
        assert_eq!(r, vec![4]);
        let e = entries(&["Calculator", "Local Security Policy"]);
        let r: Vec<usize> = rank(&e, |_| 0, "calc", 10).into_iter().map(|x| x.0).collect();
        assert_eq!(r, vec![0]);
        assert!(rank(&e, |_| 0, "  ", 10).is_empty());
    }

    #[test]
    fn keywords_and_use_bonus() {
        let e = vec![Entry::new("Display", &["screen".into(), "resolution".into()]), Entry::new("Screen saver", &[])];
        let r: Vec<usize> = rank(&e, |_| 0, "screen", 10).into_iter().map(|x| x.0).collect();
        assert_eq!(r, vec![1, 0]);
        let e = entries(&["Paint", "Paint 3D"]);
        let r: Vec<usize> = rank(&e, |i| if i == 1 { use_bonus(5) } else { 0 }, "pa", 10).into_iter().map(|x| x.0).collect();
        assert_eq!(r, vec![1, 0]);
        let r: Vec<usize> = rank(&e, |_| 0, "pa", 10).into_iter().map(|x| x.0).collect();
        assert_eq!(r, vec![0, 1]);
    }

    #[test]
    fn calculator() {
        assert_eq!(calc("2+3*4"), Some(14.0));
        assert_eq!(calc("(2+3)*4"), Some(20.0));
        assert_eq!(calc("2^10"), Some(1024.0));
        assert_eq!(calc("1,5 x 2"), Some(3.0));
        assert_eq!(calc("-3+1"), Some(-2.0));
        assert_eq!(calc("50%"), Some(0.5));
        assert_eq!(calc("12"), None);
        assert_eq!(calc("paint"), None);
        assert_eq!(calc("1/0"), None);
        assert_eq!(calc("2+"), None);
        assert_eq!(format_number(1.0 / 3.0, false), "0.333333333333");
        assert_eq!(format_number(14.0, false), "14");
        assert_eq!(format_number(2.5, true), "2,5");
    }

    #[test]
    fn rows_put_best_first_and_group() {
        let r = rows(false, &[(3, 4000), (1, 3000)], &[(7, 3500)], &[], &[2]);
        assert_eq!(r[0], Row::BestHeader);
        assert_eq!(r[1], Row::Item(Hit { kind: Kind::App, index: 3, score: 4000 }));
        assert_eq!(r[2], Row::Header(Kind::App));
        assert_eq!(r[3], Row::Item(Hit { kind: Kind::App, index: 1, score: 3000 }));
        assert_eq!(r[4], Row::Header(Kind::Setting));
        assert_eq!(r[6], Row::Header(Kind::File));
        assert_eq!(r.len(), 8);
        let r = rows(true, &[(3, 4000)], &[], &[], &[]);
        assert_eq!(r[1], Row::Item(Hit { kind: Kind::Calc, index: 0, score: u32::MAX }));
        assert_eq!(next_item(&r, Some(1), 1), Some(3));
        assert_eq!(next_item(&r, Some(3), 1), Some(3));
        assert_eq!(next_item(&r, Some(3), -1), Some(1));
        assert_eq!(next_item(&r, None, 1), Some(1));
        let r = rows(false, &[], &[], &[], &[4, 5]);
        assert_eq!(r[1], Row::Item(Hit { kind: Kind::File, index: 4, score: 0 }));
        assert_eq!(r.len(), 4);
    }

    #[test]
    fn usage_and_sql() {
        let u = vec![("MSEdge".to_string(), 3), ("a!b".to_string(), 1)];
        assert_eq!(usage_from_tsv(&usage_to_tsv(&u)), u);
        let sql = index_sql("rep 'x", 20).unwrap();
        assert!(sql.contains("CONTAINS(System.ItemNameDisplay, '\"rep*\"') AND CONTAINS(System.ItemNameDisplay, '\"x*\"')"), "{sql}");
        assert!(index_sql("  ", 20).is_none());
    }
}
