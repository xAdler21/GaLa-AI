//! Bepreistes LV als Excel-Datei.
//!
//! GP und Summen sind echte Formeln (mit vorberechnetem Ergebnis), damit man in Excel EPs ändern
//! kann und alles weiterrechnet. Titelsummen nutzen `SUBTOTAL(9; …)`: Das ignoriert verschachtelte
//! Untertitel-Summen im Bereich, nichts wird doppelt gezählt.

use lv_model::{Decimal, Lv, Node, Position, PositionsArt, RoundingStrategy, Titel};
use rust_decimal::prelude::ToPrimitive;
use rust_xlsxwriter::{Color, Format, FormatAlign, FormatBorder, Formula, Workbook, Worksheet};

use crate::ExportError;

#[derive(Debug, Clone)]
pub struct XlsxOptions {
    /// Langtext als eigene Spalte ausgeben.
    pub langtext: bool,
    /// Mehrwertsteuersatz in Prozent für die Summenzeilen; `None` = nur netto.
    pub vat_percent: Option<Decimal>,
}

impl Default for XlsxOptions {
    fn default() -> Self {
        Self {
            langtext: false,
            vat_percent: Some(Decimal::from(19)),
        }
    }
}

/// Spaltenbelegung; der Langtext ist optional und verschiebt alles rechts davon.
struct Cols {
    oz: u16,
    kurztext: u16,
    langtext: Option<u16>,
    menge: u16,
    einheit: u16,
    ep: u16,
    gp: u16,
    hinweis: u16,
}

impl Cols {
    fn new(langtext: bool) -> Self {
        let shift = u16::from(langtext);
        Cols {
            oz: 0,
            kurztext: 1,
            langtext: langtext.then_some(2),
            menge: 2 + shift,
            einheit: 3 + shift,
            ep: 4 + shift,
            gp: 5 + shift,
            hinweis: 6 + shift,
        }
    }
}

struct Formats {
    title: Format,
    header: Format,
    titel: Format,
    titel_sum: Format,
    text: Format,
    remark: Format,
    menge: Format,
    money: Format,
    hint: Format,
    total_label: Format,
    total: Format,
}

impl Formats {
    fn new() -> Self {
        let money = "#,##0.00";
        let grey = Color::RGB(0xEEEEEE);
        Formats {
            title: Format::new().set_bold().set_font_size(14),
            header: Format::new()
                .set_bold()
                .set_background_color(Color::RGB(0xD9E1F2))
                .set_border_bottom(FormatBorder::Thin),
            titel: Format::new().set_bold().set_background_color(grey),
            titel_sum: Format::new()
                .set_bold()
                .set_num_format(money)
                .set_border_top(FormatBorder::Thin),
            text: Format::new().set_text_wrap().set_align(FormatAlign::Top),
            remark: Format::new()
                .set_italic()
                .set_text_wrap()
                .set_align(FormatAlign::Top),
            menge: Format::new()
                .set_num_format("#,##0.000")
                .set_align(FormatAlign::Top),
            money: Format::new()
                .set_num_format(money)
                .set_align(FormatAlign::Top),
            hint: Format::new()
                .set_italic()
                .set_font_color(Color::RGB(0x7F7F7F))
                .set_align(FormatAlign::Top),
            total_label: Format::new().set_bold(),
            total: Format::new()
                .set_bold()
                .set_num_format(money)
                .set_border_top(FormatBorder::Double),
        }
    }
}

const HEADER_ROW: u32 = 2;

/// Schreibt das LV als `.xlsx` in einen Puffer.
pub fn lv_to_xlsx(lv: &Lv, options: &XlsxOptions) -> Result<Vec<u8>, ExportError> {
    let cols = Cols::new(options.langtext);
    let fmt = Formats::new();
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("LV")?;

    sheet.write_string_with_format(0, 0, &lv.name, &fmt.title)?;
    let (ep_label, gp_label) = (
        format!("EP [{}]", lv.currency),
        format!("GP [{}]", lv.currency),
    );
    let mut headers = vec![(cols.oz, "OZ", 14.0), (cols.kurztext, "Kurztext", 60.0)];
    if let Some(col) = cols.langtext {
        headers.push((col, "Langtext", 80.0));
    }
    headers.extend([
        (cols.menge, "Menge", 12.0),
        (cols.einheit, "Einheit", 8.0),
        (cols.ep, ep_label.as_str(), 13.0),
        (cols.gp, gp_label.as_str(), 15.0),
        (cols.hinweis, "Hinweis", 26.0),
    ]);
    for (col, label, width) in &headers {
        sheet.write_string_with_format(HEADER_ROW, *col, *label, &fmt.header)?;
        sheet.set_column_width(*col, *width)?;
    }

    let mut writer = SheetWriter {
        sheet,
        cols: &cols,
        fmt: &fmt,
        row: HEADER_ROW + 1,
    };
    let first = writer.row;
    let net = writer.nodes(&lv.nodes)?;
    let last = writer.row.saturating_sub(1).max(first);
    writer.totals(first, last, net, options.vat_percent)?;

    let sheet = writer.sheet;
    sheet.set_freeze_panes(HEADER_ROW + 1, 0)?;
    sheet.autofilter(HEADER_ROW, 0, last, cols.hinweis)?;
    sheet.set_landscape();
    sheet.set_repeat_rows(HEADER_ROW, HEADER_ROW)?;
    sheet.set_header("&L&\"-,Bold\"&A&R&D");
    sheet.set_footer("&CSeite &P von &N");

    Ok(workbook.save_to_buffer()?)
}

struct SheetWriter<'a> {
    sheet: &'a mut Worksheet,
    cols: &'a Cols,
    fmt: &'a Formats,
    row: u32,
}

impl SheetWriter<'_> {
    /// Schreibt Knoten und liefert die Summe der GP, die in die Angebotssumme eingehen.
    fn nodes(&mut self, nodes: &[Node]) -> Result<Decimal, ExportError> {
        let mut sum = Decimal::ZERO;
        for node in nodes {
            sum += match node {
                Node::Titel(titel) => self.titel(titel)?,
                Node::Position(position) => self.position(position)?,
                Node::Vorbemerkung(remark) => {
                    let text = remark.text.value.as_deref().unwrap_or_default();
                    self.sheet.write_string_with_format(
                        self.row,
                        self.cols.kurztext,
                        text,
                        &self.fmt.remark,
                    )?;
                    self.row += 1;
                    Decimal::ZERO
                }
            };
        }
        Ok(sum)
    }

    fn titel(&mut self, titel: &Titel) -> Result<Decimal, ExportError> {
        let oz = titel
            .oz
            .value
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let label = titel.bezeichnung.value.as_deref().unwrap_or_default();
        for col in [self.cols.oz, self.cols.kurztext] {
            let value = if col == self.cols.oz {
                oz.as_str()
            } else {
                label
            };
            self.sheet
                .write_string_with_format(self.row, col, value, &self.fmt.titel)?;
        }
        self.row += 1;

        let first = self.row;
        let sum = self.nodes(&titel.children)?;
        let last = self.row.saturating_sub(1).max(first);

        self.sheet.write_string_with_format(
            self.row,
            self.cols.kurztext,
            format!("Summe {oz} {label}").trim_end(),
            &self.fmt.total_label,
        )?;
        let gp = col_name(self.cols.gp);
        let formula = format!("=SUBTOTAL(9,{gp}{}:{gp}{})", first + 1, last + 1);
        self.sheet.write_formula_with_format(
            self.row,
            self.cols.gp,
            Formula::new(formula).set_result(sum.to_string()),
            &self.fmt.titel_sum,
        )?;
        self.row += 2;
        Ok(sum)
    }

    fn position(&mut self, p: &Position) -> Result<Decimal, ExportError> {
        let (row, cols, fmt) = (self.row, self.cols, self.fmt);
        let oz = p.oz.value.as_ref().map(ToString::to_string);
        let oz = oz.as_deref().or(p.oz.raw.as_deref()).unwrap_or_default();
        self.sheet
            .write_string_with_format(row, cols.oz, oz, &fmt.text)?;
        self.sheet.write_string_with_format(
            row,
            cols.kurztext,
            p.kurztext.value.as_deref().unwrap_or_default(),
            &fmt.text,
        )?;
        if let (Some(col), Some(text)) = (cols.langtext, p.langtext.value.as_deref()) {
            self.sheet
                .write_string_with_format(row, col, text, &fmt.text)?;
        }
        if let Some(menge) = p.menge.value {
            self.sheet
                .write_number_with_format(row, cols.menge, to_f64(menge), &fmt.menge)?;
        }
        let einheit = p
            .einheit
            .value
            .map(|u| u.symbol().to_string())
            .or_else(|| p.einheit.raw.clone());
        if let Some(einheit) = einheit {
            self.sheet
                .write_string_with_format(row, cols.einheit, einheit, &fmt.text)?;
        }
        if let Some(ep) = p.ep.value {
            self.sheet
                .write_number_with_format(row, cols.ep, to_f64(ep), &fmt.money)?;
        }

        let hint = match p.art {
            PositionsArt::Normal => None,
            PositionsArt::Bedarf => Some("Bedarfsposition (nur EP)".to_string()),
            PositionsArt::Alternativ => Some(match &p.grund_oz {
                Some(base) => format!("Alternative zu {base} (nur EP)"),
                None => "Alternativposition (nur EP)".to_string(),
            }),
            PositionsArt::Zulage => Some("Zulage".to_string()),
            PositionsArt::Stundenlohn => Some("Stundenlohnarbeiten".to_string()),
        };
        if let Some(hint) = hint {
            self.sheet
                .write_string_with_format(row, cols.hinweis, hint, &fmt.hint)?;
        }

        let mut gp = Decimal::ZERO;
        if p.art.counts_towards_total() {
            let (ep_ref, menge_ref) = (cell(cols.ep, row), cell(cols.menge, row));
            // Ohne Menge (Pauschale) ist der GP der EP.
            let (formula, value) = match p.menge.value {
                Some(_) => (format!("=ROUND({menge_ref}*{ep_ref},2)"), p.computed_gp()),
                None => (format!("=ROUND({ep_ref},2)"), p.ep.value.map(round2)),
            };
            gp = value.unwrap_or(Decimal::ZERO);
            self.sheet.write_formula_with_format(
                row,
                cols.gp,
                Formula::new(formula).set_result(gp.to_string()),
                &fmt.money,
            )?;
        }
        self.row += 1;
        Ok(gp)
    }

    fn totals(
        &mut self,
        first: u32,
        last: u32,
        net: Decimal,
        vat_percent: Option<Decimal>,
    ) -> Result<(), ExportError> {
        let (cols, fmt) = (self.cols, self.fmt);
        let gp = col_name(cols.gp);
        let net_row = self.row + 1;
        self.sheet.write_string_with_format(
            net_row,
            cols.kurztext,
            "Angebotssumme netto",
            &fmt.total_label,
        )?;
        self.sheet.write_formula_with_format(
            net_row,
            cols.gp,
            Formula::new(format!("=SUBTOTAL(9,{gp}{}:{gp}{})", first + 1, last + 1))
                .set_result(net.to_string()),
            &fmt.total,
        )?;
        self.row = net_row + 1;

        if let Some(rate) = vat_percent {
            let vat = round2(net * rate / Decimal::ONE_HUNDRED);
            let net_cell = cell(cols.gp, net_row);
            self.sheet.write_string_with_format(
                self.row,
                cols.kurztext,
                format!("zzgl. {} % MwSt.", rate.normalize()),
                &fmt.total_label,
            )?;
            self.sheet.write_formula_with_format(
                self.row,
                cols.gp,
                Formula::new(format!("=ROUND({net_cell}*{rate}/100,2)"))
                    .set_result(vat.to_string()),
                &fmt.money,
            )?;
            let vat_cell = cell(cols.gp, self.row);
            self.row += 1;
            self.sheet.write_string_with_format(
                self.row,
                cols.kurztext,
                "Angebotssumme brutto",
                &fmt.total_label,
            )?;
            self.sheet.write_formula_with_format(
                self.row,
                cols.gp,
                Formula::new(format!("={net_cell}+{vat_cell}")).set_result((net + vat).to_string()),
                &fmt.total,
            )?;
            self.row += 1;
        }
        Ok(())
    }
}

fn round2(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

fn to_f64(value: Decimal) -> f64 {
    value.to_f64().unwrap_or_default()
}

/// Spaltenbuchstabe für die ersten 26 Spalten (mehr braucht das Blatt nicht).
fn col_name(col: u16) -> char {
    char::from(b'A' + u8::try_from(col).unwrap_or(25).min(25))
}

/// A1-Bezug einer Zelle (0-basierte Zeile/Spalte).
fn cell(col: u16, row: u32) -> String {
    format!("{}{}", col_name(col), row + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{Data, Range, Reader, Xlsx};
    use lv_model::{Field, LvSource, Oz, Unit, Vorbemerkung};
    use rust_decimal_macros::dec;

    fn position(
        oz: &str,
        menge: Option<Decimal>,
        unit: Unit,
        ep: Decimal,
        art: PositionsArt,
    ) -> Node {
        let mut p = Position::new(Oz::parse(oz).unwrap(), format!("Kurztext {oz}"));
        p.langtext = Field::certain(format!("Langtext {oz}"));
        p.menge = menge.map_or_else(Field::absent, Field::certain);
        p.einheit = Field::certain(unit);
        p.ep = Field::certain(ep);
        p.art = art;
        Node::Position(Box::new(p))
    }

    fn titel(oz: &str, label: &str, children: Vec<Node>) -> Node {
        Node::Titel(Titel {
            oz: Field::certain(Oz::parse(oz).unwrap()),
            bezeichnung: Field::certain(label.to_string()),
            children,
        })
    }

    fn sample() -> Lv {
        Lv {
            name: "Außenanlagen Kita".into(),
            source: LvSource::Manual,
            currency: "EUR".into(),
            oz_mask: None,
            nodes: vec![
                titel(
                    "01",
                    "Pflasterarbeiten",
                    vec![
                        Node::Vorbemerkung(Vorbemerkung {
                            text: Field::certain("Gefälle 2,5 %".into()),
                        }),
                        position(
                            "01.0010",
                            Some(dec!(120)),
                            Unit::M2,
                            dec!(45.50),
                            PositionsArt::Normal,
                        ),
                        position(
                            "01.0020",
                            Some(dec!(15)),
                            Unit::M,
                            dec!(38.20),
                            PositionsArt::Bedarf,
                        ),
                        titel(
                            "01.02",
                            "Baustelle",
                            vec![position(
                                "01.02.0010",
                                None,
                                Unit::Psch,
                                dec!(1500),
                                PositionsArt::Normal,
                            )],
                        ),
                    ],
                ),
                position(
                    "02.0010",
                    Some(dec!(10)),
                    Unit::H,
                    dec!(58),
                    PositionsArt::Stundenlohn,
                ),
            ],
        }
    }

    fn open(bytes: Vec<u8>) -> (Range<Data>, Range<String>) {
        let mut wb: Xlsx<_> = Xlsx::new(std::io::Cursor::new(bytes)).unwrap();
        (
            wb.worksheet_range("LV").unwrap(),
            wb.worksheet_formula("LV").unwrap(),
        )
    }

    /// Zeile, deren Kurztext-Spalte (B) mit `prefix` beginnt.
    fn row_of(range: &Range<Data>, prefix: &str) -> u32 {
        (0..=range.end().unwrap().0)
            .find(|&r| matches!(range.get_value((r, 1)), Some(Data::String(s)) if s.starts_with(prefix)))
            .unwrap_or_else(|| panic!("Zeile {prefix:?} fehlt"))
    }

    fn number(range: &Range<Data>, row: u32, col: u32) -> f64 {
        match range.get_value((row, col)) {
            Some(Data::Float(f)) => *f,
            Some(Data::Int(i)) => *i as f64,
            other => panic!("Zahl erwartet in ({row}, {col}), gefunden {other:?}"),
        }
    }

    #[test]
    fn writes_positions_with_formulas_and_totals() {
        let (values, formulas) = open(lv_to_xlsx(&sample(), &XlsxOptions::default()).unwrap());
        // Spalten ohne Langtext: A OZ, B Kurztext, C Menge, D Einheit, E EP, F GP, G Hinweis
        assert_eq!(
            values.get_value((0, 0)),
            Some(&Data::String("Außenanlagen Kita".into()))
        );
        assert_eq!(
            values.get_value((2, 5)),
            Some(&Data::String("GP [EUR]".into()))
        );

        let pflaster = row_of(&values, "Kurztext 01.0010");
        assert_eq!(
            values.get_value((pflaster, 0)),
            Some(&Data::String("01.0010".into()))
        );
        assert_eq!(number(&values, pflaster, 2), 120.0);
        assert_eq!(
            values.get_value((pflaster, 3)),
            Some(&Data::String("m2".into()))
        );
        assert_eq!(number(&values, pflaster, 5), 5460.0);
        let r = pflaster + 1;
        assert_eq!(
            formulas.get_value((pflaster, 5)).map(String::as_str),
            Some(&*format!("ROUND(C{r}*E{r},2)"))
        );

        let bedarf = row_of(&values, "Kurztext 01.0020");
        assert!(matches!(
            values.get_value((bedarf, 5)),
            None | Some(Data::Empty)
        ));
        assert_eq!(
            values.get_value((bedarf, 6)),
            Some(&Data::String("Bedarfsposition (nur EP)".into()))
        );

        let pauschale = row_of(&values, "Kurztext 01.02.0010");
        assert_eq!(number(&values, pauschale, 5), 1500.0);

        // Titel 01: 5460 + 1500, Bedarf zählt nicht; Untertitel-Summe nicht doppelt.
        let summe_01 = row_of(&values, "Summe 01 Pflasterarbeiten");
        assert_eq!(number(&values, summe_01, 5), 6960.0);
        assert!(
            formulas
                .get_value((summe_01, 5))
                .unwrap()
                .starts_with("SUBTOTAL(9,")
        );

        let netto = row_of(&values, "Angebotssumme netto");
        assert_eq!(number(&values, netto, 5), 7540.0);
        assert_eq!(
            number(&values, row_of(&values, "zzgl. 19 % MwSt."), 5),
            1432.6
        );
        assert_eq!(
            number(&values, row_of(&values, "Angebotssumme brutto"), 5),
            8972.6
        );
    }

    #[test]
    fn optional_langtext_column_shifts_layout() {
        let options = XlsxOptions {
            langtext: true,
            vat_percent: None,
        };
        let (values, _) = open(lv_to_xlsx(&sample(), &options).unwrap());
        assert_eq!(
            values.get_value((2, 2)),
            Some(&Data::String("Langtext".into()))
        );
        let pflaster = row_of(&values, "Kurztext 01.0010");
        assert_eq!(
            values.get_value((pflaster, 2)),
            Some(&Data::String("Langtext 01.0010".into()))
        );
        assert_eq!(number(&values, pflaster, 6), 5460.0);

        let rows = values.end().unwrap().0;
        assert!((0..=rows).all(
            |r| !matches!(values.get_value((r, 1)), Some(Data::String(s)) if s.contains("MwSt"))
        ));
    }
}
