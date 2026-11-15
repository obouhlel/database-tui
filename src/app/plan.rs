//! Readable rendering of `EXPLAIN (FORMAT JSON)` plans: one line per node
//! in a tree, with estimated cost and rows, and with `ANALYZE` the actual
//! time, rows and loops. Each node gets its share of the work (exclusive
//! time with ANALYZE, else exclusive cost) to point at the expensive parts.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct PlanLine {
    /// Tree drawing before the node (`│  ├─ `).
    pub prefix: String,
    /// `Seq Scan on shop.orders o`
    pub title: String,
    /// `cost 0.00..1834.00 · 100 000 lignes estimées`
    pub stats: String,
    /// Conditions, sort keys, ... (one line each).
    pub details: Vec<String>,
    /// Share of the work done by this node alone, 0..=1.
    pub share: f64,
    /// Estimated rows far from actual rows (x10 or more).
    pub misestimate: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    pub lines: Vec<PlanLine>,
    /// Planning / execution time and the like.
    pub summary: Vec<String>,
    pub analyze: bool,
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| match x {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => Some(
            a.iter()
                .map(|e| e.as_str().map_or_else(|| e.to_string(), str::to_string))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Value::Null => None,
        other => Some(other.to_string()),
    })
}

/// `1234567.0` -> `1 234 567`
fn rows(n: f64) -> String {
    super::format::count(n.round() as i64)
}

fn ms(x: f64) -> String {
    if x >= 1000.0 {
        format!("{:.2} s", x / 1000.0)
    } else if x >= 10.0 {
        format!("{x:.1} ms")
    } else {
        format!("{x:.3} ms")
    }
}

/// Total work of a node (inclusive), for the share computation.
fn inclusive(node: &Value, analyze: bool) -> f64 {
    if analyze {
        num(node, "Actual Total Time").unwrap_or(0.0) * num(node, "Actual Loops").unwrap_or(1.0)
    } else {
        num(node, "Total Cost").unwrap_or(0.0)
    }
}

pub fn parse(json: &str) -> Result<Plan, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("plan illisible : {e}"))?;
    let root = v.get(0).ok_or_else(|| "plan vide".to_string())?;
    let top = root
        .get("Plan")
        .ok_or_else(|| "plan sans nœud".to_string())?;
    let analyze = top.get("Actual Total Time").is_some();
    let total = inclusive(top, analyze).max(f64::EPSILON);
    let mut plan = Plan {
        analyze,
        ..Default::default()
    };
    walk(top, "", "", true, analyze, total, &mut plan.lines);
    if let Some(p) = num(root, "Planning Time") {
        plan.summary.push(format!("Planification : {}", ms(p)));
    }
    if let Some(e) = num(root, "Execution Time") {
        plan.summary.push(format!("Exécution : {}", ms(e)));
    }
    if let Some(triggers) = root.get("Triggers").and_then(Value::as_array) {
        for t in triggers {
            plan.summary.push(format!(
                "Trigger {} : {} appel(s), {}",
                text(t, "Trigger Name").unwrap_or_default(),
                num(t, "Calls").unwrap_or(0.0),
                ms(num(t, "Time").unwrap_or(0.0))
            ));
        }
    }
    Ok(plan)
}

fn walk(
    node: &Value,
    lead: &str,
    branch: &str,
    root: bool,
    analyze: bool,
    total: f64,
    out: &mut Vec<PlanLine>,
) {
    let kind = text(node, "Node Type").unwrap_or_else(|| "?".into());
    let mut title = kind.clone();
    if let Some(s) = text(node, "Strategy").filter(|s| s != "Plain" && kind == "Aggregate") {
        title = format!("{s} {title}");
    }
    if let Some(j) =
        text(node, "Join Type").filter(|_| kind.contains("Join") || kind == "Nested Loop")
    {
        title = format!("{title} ({j})");
    }
    if let Some(idx) = text(node, "Index Name") {
        title.push_str(&format!(" using {idx}"));
    }
    if let Some(rel) = text(node, "Relation Name") {
        let schema = text(node, "Schema")
            .map(|s| format!("{s}."))
            .unwrap_or_default();
        title.push_str(&format!(" on {schema}{rel}"));
        if let Some(alias) = text(node, "Alias").filter(|a| *a != rel) {
            title.push_str(&format!(" {alias}"));
        }
    } else if let Some(name) = text(node, "CTE Name").or_else(|| text(node, "Function Name")) {
        title.push_str(&format!(" on {name}"));
    }
    if let Some(rel) =
        text(node, "Parent Relationship").filter(|r| r == "InitPlan" || r == "SubPlan")
    {
        title = format!("{rel} : {title}");
    }

    let est = num(node, "Plan Rows").unwrap_or(0.0);
    let mut stats = format!(
        "cost {:.2}..{:.2} · {} ligne(s) estimée(s)",
        num(node, "Startup Cost").unwrap_or(0.0),
        num(node, "Total Cost").unwrap_or(0.0),
        rows(est)
    );
    let mut misestimate = false;
    if analyze {
        match (num(node, "Actual Rows"), num(node, "Actual Loops")) {
            (Some(actual), Some(loops)) if loops > 0.0 => {
                let time = num(node, "Actual Total Time").unwrap_or(0.0);
                stats.push_str(&format!(
                    " · réel {} ligne(s) en {}",
                    rows(actual),
                    ms(time)
                ));
                if loops > 1.0 {
                    stats.push_str(&format!(" × {} boucles", rows(loops)));
                }
                let (a, e) = (actual.max(1.0), est.max(1.0));
                misestimate = a / e >= 10.0 || e / a >= 10.0;
            }
            _ => stats.push_str(" · jamais exécuté"),
        }
    }

    let mut details = Vec::new();
    for (key, label) in [
        ("Index Cond", "index"),
        ("Recheck Cond", "revérification"),
        ("Hash Cond", "hachage"),
        ("Merge Cond", "fusion"),
        ("Join Filter", "filtre de jointure"),
        ("Filter", "filtre"),
        ("Sort Key", "tri"),
        ("Group Key", "groupement"),
        ("Presorted Key", "déjà trié"),
    ] {
        if let Some(t) = text(node, key) {
            details.push(format!("{label} : {t}"));
        }
    }
    if let Some(n) = num(node, "Rows Removed by Filter").filter(|n| *n > 0.0) {
        details.push(format!("{} ligne(s) écartée(s) par le filtre", rows(n)));
    }
    if let Some(m) = text(node, "Sort Method") {
        let space = num(node, "Sort Space Used")
            .map(|k| format!(", {k} kB"))
            .unwrap_or_default();
        let kind = text(node, "Sort Space Type")
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        details.push(format!("méthode de tri : {m}{space}{kind}"));
    }
    if let Some(b) = num(node, "Hash Batches").filter(|b| *b > 1.0) {
        details.push(format!("hachage en {b} lots (mémoire insuffisante)"));
    }
    if let Some(w) = num(node, "Workers Launched") {
        details.push(format!("{w} worker(s) parallèle(s)"));
    }

    let children: Vec<&Value> = node
        .get("Plans")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let own =
        inclusive(node, analyze) - children.iter().map(|c| inclusive(c, analyze)).sum::<f64>();
    out.push(PlanLine {
        prefix: format!("{lead}{branch}"),
        title,
        stats,
        details,
        share: (own / total).clamp(0.0, 1.0),
        misestimate,
    });
    let child_lead = if root {
        String::new()
    } else {
        format!(
            "{lead}{}",
            if branch.starts_with('└') {
                "   "
            } else {
                "│  "
            }
        )
    };
    for (i, c) in children.iter().enumerate() {
        let last = i + 1 == children.len();
        walk(
            c,
            &child_lead,
            if last { "└─ " } else { "├─ " },
            false,
            analyze,
            total,
            out,
        );
    }
}

impl Plan {
    /// Plain text (for copying).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            out.push_str(&format!("{}{}  ({})\n", l.prefix, l.title, l.stats));
            for d in &l.details {
                out.push_str(&format!(
                    "{}   {d}\n",
                    l.prefix.replace(['├', '└'], " ").replace('─', " ")
                ));
            }
        }
        for s in &self.summary {
            out.push_str(s);
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const ANALYZE: &str = r#"[{"Plan": {"Node Type": "Hash Join", "Join Type": "Inner",
        "Startup Cost": 30.0, "Total Cost": 2500.0, "Plan Rows": 100, "Actual Total Time": 50.0,
        "Actual Rows": 5000, "Actual Loops": 1, "Hash Cond": "(o.customer_id = c.id)",
        "Plans": [
          {"Node Type": "Seq Scan", "Parent Relationship": "Outer", "Relation Name": "orders",
           "Schema": "shop", "Alias": "o", "Startup Cost": 0.0, "Total Cost": 1800.0, "Plan Rows": 100000,
           "Actual Total Time": 40.0, "Actual Rows": 100000, "Actual Loops": 1,
           "Filter": "(total > 10)", "Rows Removed by Filter": 12},
          {"Node Type": "Hash", "Parent Relationship": "Inner", "Startup Cost": 20.0, "Total Cost": 20.0,
           "Plan Rows": 1000, "Actual Total Time": 2.0, "Actual Rows": 1000, "Actual Loops": 1,
           "Plans": [{"Node Type": "Index Scan", "Parent Relationship": "Outer", "Index Name": "customers_pkey",
             "Relation Name": "customers", "Schema": "shop", "Alias": "c", "Startup Cost": 0.0,
             "Total Cost": 18.0, "Plan Rows": 1000, "Actual Total Time": 1.5, "Actual Rows": 1000,
             "Actual Loops": 1, "Index Cond": "(id > 0)"}]}
        ]},
      "Planning Time": 0.25, "Execution Time": 51.2}]"#;

    #[test]
    fn tree_lines_and_shares() {
        let p = parse(ANALYZE).unwrap();
        assert!(p.analyze);
        let titles: Vec<String> = p
            .lines
            .iter()
            .map(|l| format!("{}{}", l.prefix, l.title))
            .collect();
        assert_eq!(
            titles,
            [
                "Hash Join (Inner)",
                "├─ Seq Scan on shop.orders o",
                "└─ Hash",
                "   └─ Index Scan using customers_pkey on shop.customers c",
            ]
        );
        // The scan does 40 of 50 ms.
        assert!((p.lines[1].share - 0.8).abs() < 1e-9);
        assert!((p.lines[0].share - 0.16).abs() < 1e-9);
        assert!(p.lines[0].misestimate, "100 estimated, 5000 actual");
        assert!(!p.lines[1].misestimate);
        assert_eq!(
            p.lines[1].details,
            [
                "filtre : (total > 10)",
                "12 ligne(s) écartée(s) par le filtre"
            ]
        );
        assert_eq!(
            p.summary,
            ["Planification : 0.250 ms", "Exécution : 51.2 ms"]
        );
        assert!(
            p.to_text()
                .contains("├─ Seq Scan on shop.orders o  (cost 0.00..1800.00")
        );
    }

    #[test]
    fn estimate_only() {
        let p = parse(
            r#"[{"Plan": {"Node Type": "Seq Scan", "Relation Name": "t", "Alias": "t",
            "Startup Cost": 0.0, "Total Cost": 35.5, "Plan Rows": 2550}}]"#,
        )
        .unwrap();
        assert!(!p.analyze);
        assert_eq!(p.lines[0].title, "Seq Scan on t");
        assert_eq!(
            p.lines[0].stats,
            "cost 0.00..35.50 · 2 550 ligne(s) estimée(s)"
        );
        assert!((p.lines[0].share - 1.0).abs() < 1e-9);
        assert!(parse("[]").is_err());
    }
}
