//! The search box syntax shared by the asset lists.
//!
//! Terms are separated by spaces or commas and all have to match. `ak47` matches a name that contains it, `!ak47` one that does
//! not. `width:>=2048`, `length<3`, `rate:44100` compare a property of the asset (`>`, `<`, `>=`, `<=`, `=` or none for equals);
//! a property name the list does not have makes the term a plain text match.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Gt,
    Lt,
    Ge,
    Le,
    Eq,
}

#[derive(Clone, Debug, PartialEq)]
enum Term {
    Text { word: String, not: bool },
    Prop { name: String, op: Op, value: f64, not: bool },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    terms: Vec<Term>,
}

fn split_op(s: &str) -> (Op, &str) {
    for (p, op) in [(">=", Op::Ge), ("<=", Op::Le), (">", Op::Gt), ("<", Op::Lt), ("==", Op::Eq), ("=", Op::Eq)] {
        if let Some(rest) = s.strip_prefix(p) {
            return (op, rest);
        }
    }
    (Op::Eq, s)
}

impl Query {
    /// Parses the search text; `props` are the property names the list understands (lower case).
    pub fn parse(text: &str, props: &[&str]) -> Query {
        let mut terms = Vec::new();
        for raw in text.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()) {
            let (not, t) = match raw.strip_prefix('!') {
                Some(r) => (true, r),
                None => (false, raw),
            };
            if t.is_empty() {
                continue;
            }
            let lower = t.to_lowercase();
            // `name:>=5` or `name>=5`
            let split = lower.find(|c: char| matches!(c, ':' | '>' | '<' | '='));
            if let Some(i) = split {
                let name = &lower[..i];
                if props.contains(&name) {
                    let rest = lower[i..].strip_prefix(':').unwrap_or(&lower[i..]);
                    let (op, num) = split_op(rest);
                    if let Ok(value) = num.parse::<f64>() {
                        terms.push(Term::Prop { name: name.to_string(), op, value, not });
                        continue;
                    }
                }
            }
            terms.push(Term::Text { word: lower, not });
        }
        Query { terms }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Whether an asset matches: `name` is its (lower case) name, `prop` gives a property value when it has one.
    pub fn matches(&self, name: &str, prop: impl Fn(&str) -> Option<f64>) -> bool {
        self.terms.iter().all(|t| match t {
            Term::Text { word, not } => name.contains(word.as_str()) != *not,
            Term::Prop { name: p, op, value, not } => {
                let Some(v) = prop(p) else { return *not };
                let ok = match op {
                    Op::Gt => v > *value,
                    Op::Lt => v < *value,
                    Op::Ge => v >= *value,
                    Op::Le => v <= *value,
                    Op::Eq => (v - *value).abs() < 1e-9,
                };
                ok != *not
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(p: &str) -> Option<f64> {
        match p {
            "width" => Some(2048.0),
            "height" => Some(512.0),
            _ => None,
        }
    }

    #[test]
    fn text_terms_all_have_to_match_and_bang_excludes() {
        let q = Query::parse("ak47, !fire", &[]);
        assert!(q.matches("weapons/ak47/idle", |_| None));
        assert!(!q.matches("weapons/ak47/fire", |_| None));
        assert!(!q.matches("weapons/m4/idle", |_| None));
    }

    #[test]
    fn properties_compare_with_every_operator() {
        let names = ["width", "height"];
        assert!(Query::parse("width:>=2048", &names).matches("x", props));
        assert!(!Query::parse("width:>2048", &names).matches("x", props));
        assert!(Query::parse("width>1000 height<=512", &names).matches("x", props));
        assert!(Query::parse("width:2048,height:512", &names).matches("x", props));
        assert!(Query::parse("!height:>600", &names).matches("x", props));
    }

    #[test]
    fn an_unknown_property_is_a_text_match_and_an_unset_one_never_matches() {
        assert!(Query::parse("foo:>3", &["width"]).matches("a foo:>3 b", |_| None));
        assert!(!Query::parse("length:>3", &["length"]).matches("x", |_| None));
    }
}
