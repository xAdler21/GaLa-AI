//! GAEB-Texte (XHTML-Teilmenge: `p`, `span`, `br`, Tabellen) in Klartext umwandeln.

use roxmltree::Node;

/// Klartext mit Zeilenumbrüchen an Absätzen und `<br/>`; Formatierungs-Leerraum im XML zählt
/// wie in HTML als einfaches Leerzeichen.
pub fn plain_text(node: Node<'_, '_>) -> String {
    let mut raw = String::new();
    collect(node, &mut raw);
    tidy(&raw)
}

/// Wie [`plain_text`], aber einzeilig (für Kurztexte und Titel).
pub fn single_line(node: Node<'_, '_>) -> String {
    plain_text(node).lines().collect::<Vec<_>>().join(" ")
}

fn collect(node: Node<'_, '_>, out: &mut String) {
    for child in node.children() {
        if child.is_text() {
            for c in child.text().unwrap_or_default().chars() {
                if c.is_whitespace() {
                    if !out.ends_with([' ', '\n']) && !out.is_empty() {
                        out.push(' ');
                    }
                } else {
                    out.push(c);
                }
            }
        } else if child.is_element() {
            match child.tag_name().name() {
                "br" => out.push('\n'),
                "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" => {
                    newline(out);
                    collect(child, out);
                    newline(out);
                }
                "td" | "th" => {
                    collect(child, out);
                    out.push(' ');
                }
                _ => collect(child, out),
            }
        }
    }
}

fn newline(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Zeilen trimmen, Mehrfach-Leerzeichen und Leerzeilen am Rand entfernen.
fn tidy(raw: &str) -> String {
    let lines: Vec<String> = raw
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let mut out: Vec<&str> = Vec::new();
    for line in &lines {
        if line.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(xml: &str) -> String {
        let doc = roxmltree::Document::parse(xml).unwrap();
        plain_text(doc.root_element())
    }

    #[test]
    fn paragraphs_and_breaks_become_lines() {
        let xml = r#"<Text>
            <p><span>Betonsteinpflaster 20x10x8 cm,</span> <span>grau,</span></p>
            <p><span>im Sandbett verlegen.</span><br/><span>Fugen   mit Brechsand verfüllen.</span></p>
        </Text>"#;
        assert_eq!(
            text_of(xml),
            "Betonsteinpflaster 20x10x8 cm, grau,\nim Sandbett verlegen.\nFugen mit Brechsand verfüllen."
        );
    }

    #[test]
    fn entities_and_tables() {
        let xml = r#"<Text><p>Wege &amp; Plätze</p><table><tr><td>Körnung</td><td>0/32</td></tr></table></Text>"#;
        assert_eq!(text_of(xml), "Wege & Plätze\nKörnung 0/32");
    }

    #[test]
    fn empty_text_stays_empty() {
        assert_eq!(text_of("<Text>  <p> </p> </Text>"), "");
    }
}
