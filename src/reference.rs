use wikibase_rest_api::prelude::*;
use wikibase_rest_api::property_value::PropertyValue;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum DataValue {
    Monolingual {
        label: String,
        language: String,
    },
    String(String),
    Entity(String),
    Date {
        time: String,
        precision: TimePrecision,
    },
    Quantity(i64),
}

impl DataValue {
    pub fn as_statement_value(&self) -> StatementValue {
        let svc = match self {
            DataValue::Monolingual { label, language } => StatementValueContent::MonolingualText {
                language: language.to_string(),
                text: label.to_string(),
            },
            DataValue::String(s) => StatementValueContent::String(s.to_string()),
            DataValue::Entity(e) => StatementValueContent::String(e.to_string()),
            DataValue::Date { time, precision } => StatementValueContent::Time {
                time: time.to_string(),
                precision: precision.to_owned(),
                calendarmodel: GREGORIAN_CALENDAR.to_string(),
            },
            // Wikibase format: signed amount, "1" for no unit
            DataValue::Quantity(amount) => StatementValueContent::Quantity {
                amount: format!("{amount:+}"),
                unit: "1".to_string(),
            },
        };
        StatementValue::Value(svc)
    }

    /// How an existing statement value relates to this one. Compared by
    /// meaning, as Wikidata formats some values differently, e.g. "+0042" or
    /// a year as "+2000-00-00T00:00:00Z".
    pub fn compare(&self, existing: &StatementValue) -> ValueMatch {
        let StatementValue::Value(existing) = existing else {
            return ValueMatch::Different;
        };
        let same = match (self, existing) {
            (DataValue::Quantity(n), StatementValueContent::Quantity { amount, unit }) => {
                unit == "1" && amount.parse::<i64>() == Ok(*n)
            }
            (
                DataValue::Date { time, precision },
                StatementValueContent::Time {
                    time: existing_time,
                    precision: existing_precision,
                    calendarmodel,
                },
            ) => {
                return Self::compare_dates(
                    (time, *precision),
                    (existing_time, *existing_precision),
                    calendarmodel,
                );
            }
            _ => StatementValue::Value(existing.to_owned()) == self.as_statement_value(),
        };
        if same {
            ValueMatch::Same
        } else {
            ValueMatch::Different
        }
    }

    fn compare_dates(
        (time, precision): (&str, TimePrecision),
        (existing_time, existing_precision): (&str, TimePrecision),
        calendarmodel: &str,
    ) -> ValueMatch {
        if calendarmodel != GREGORIAN_CALENDAR || existing_precision < precision {
            return ValueMatch::Different;
        }
        let same_at_precision = match Self::date_parts(time, precision) {
            Some(parts) => Self::date_parts(existing_time, precision) == Some(parts),
            None => time == existing_time, // Unusual precision; require identity
        };
        match (same_at_precision, existing_precision == precision) {
            (false, _) => ValueMatch::Different,
            (true, true) => ValueMatch::Same,
            (true, false) => ValueMatch::MorePrecise,
        }
    }

    /// Sign plus the year/month/day components significant at `precision`.
    fn date_parts(time: &str, precision: TimePrecision) -> Option<Vec<&str>> {
        let n = match precision {
            TimePrecision::Year => 1,
            TimePrecision::Month => 2,
            TimePrecision::Day => 3,
            _ => return None,
        };
        let sign = time.get(..1)?;
        let parts = time.get(1..)?.split(['-', 'T']).take(n);
        Some(std::iter::once(sign).chain(parts).collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueMatch {
    Same,
    /// Same value, stated more precisely (e.g. a full date for a year)
    MorePrecise,
    Different,
}

#[derive(Debug, Clone, Default, Hash, PartialEq, Eq)]
pub struct Reference {
    property: Option<String>,
    value: Option<String>,
    url: Option<String>,
}

impl Reference {
    pub fn prop(property: &str, value: &str) -> Self {
        Reference {
            property: Some(property.to_string()),
            value: Some(value.to_string()),
            url: None,
        }
    }

    pub const fn none() -> Self {
        Reference {
            property: None,
            value: None,
            url: None,
        }
    }

    #[cfg(test)]
    fn _url(url: &str) -> Self {
        Reference {
            property: None,
            value: None,
            url: Some(url.to_string()),
        }
    }

    pub fn is_equivalent(&self, reference: &wikibase_rest_api::Reference) -> bool {
        let (expected_prop, expected_value) = match (&self.property, &self.value, &self.url) {
            (Some(property), Some(value), _) => (property.as_str(), value.as_str()),
            (_, _, Some(url)) => ("P854", url.as_str()),
            _ => return false,
        };
        reference.parts().iter().any(|prop_value| {
            prop_value.property().id() == expected_prop
                && matches!(
                    prop_value.value(),
                    StatementValue::Value(StatementValueContent::String(s)) if s == expected_value
                )
        })
    }

    pub fn as_ref_group(&self) -> Option<wikibase_rest_api::Reference> {
        let mut ret = wikibase_rest_api::Reference::default();
        if let (Some(property), Some(value)) = (&self.property, &self.value) {
            let p = PropertyType::new(
                property.to_owned(),
                Some(wikibase_rest_api::DataType::String),
            );
            let v = StatementValue::Value(StatementValueContent::String(value.to_owned()));
            let pv = PropertyValue::new(p, v);
            ret.parts_mut().push(pv);
        } else if let Some(url) = &self.url {
            let p = PropertyType::new("P854", Some(wikibase_rest_api::DataType::Url));
            let v = StatementValue::Value(StatementValueContent::String(url.to_owned()));
            let pv = PropertyValue::new(p, v);
            ret.parts_mut().push(pv);
        } else {
            return None;
        }

        let p = PropertyType::new("P813", Some(wikibase_rest_api::DataType::Time));
        let v = StatementValue::Value(StatementValueContent::Time {
            time: chrono::Utc::now().format("+%Y-%m-%dT00:00:00Z").to_string(),
            precision: TimePrecision::Day,
            calendarmodel: GREGORIAN_CALENDAR.to_string(),
        });
        let pv = PropertyValue::new(p, v);
        ret.parts_mut().push(pv);
        Some(ret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── DataValue::as_statement_value ─────────────────────────────────────────

    #[test]
    fn test_as_statement_value_string() {
        let sv = DataValue::String("hello".to_string()).as_statement_value();
        assert!(
            matches!(
                sv,
                StatementValue::Value(StatementValueContent::String(s)) if s == "hello"
            ),
            "String variant should produce a String StatementValue"
        );
    }

    #[test]
    fn test_as_statement_value_entity_is_string() {
        // Entity is intentionally serialised as a plain String value
        let sv = DataValue::Entity("Q42".to_string()).as_statement_value();
        assert!(
            matches!(
                sv,
                StatementValue::Value(StatementValueContent::String(s)) if s == "Q42"
            ),
            "Entity variant should produce a String StatementValue containing the QID"
        );
    }

    #[test]
    fn test_as_statement_value_monolingual() {
        let sv = DataValue::Monolingual {
            label: "Hello".to_string(),
            language: "en".to_string(),
        }
        .as_statement_value();
        assert!(
            matches!(
                sv,
                StatementValue::Value(StatementValueContent::MonolingualText { language, text })
                    if language == "en" && text == "Hello"
            ),
            "Monolingual variant should produce a MonolingualText StatementValue"
        );
    }

    #[test]
    fn test_as_statement_value_quantity() {
        let sv = DataValue::Quantity(42).as_statement_value();
        assert!(
            matches!(
                sv,
                StatementValue::Value(StatementValueContent::Quantity { amount, unit })
                    if amount == "+42" && unit == "1"
            ),
            "Quantity variant should produce a Quantity StatementValue"
        );
    }

    #[test]
    fn test_as_statement_value_date() {
        let sv = DataValue::Date {
            time: "+2000-01-01T00:00:00Z".to_string(),
            precision: TimePrecision::Year,
        }
        .as_statement_value();
        assert!(
            matches!(
                sv,
                StatementValue::Value(StatementValueContent::Time { time, precision, .. })
                    if time == "+2000-01-01T00:00:00Z"
                    && precision == TimePrecision::Year
            ),
            "Date variant should produce a Time StatementValue with the correct time and precision"
        );
    }

    #[test]
    fn test_as_statement_value_date_calendarmodel() {
        let sv = DataValue::Date {
            time: "+2000-01-01T00:00:00Z".to_string(),
            precision: TimePrecision::Day,
        }
        .as_statement_value();
        if let StatementValue::Value(StatementValueContent::Time { calendarmodel, .. }) = sv {
            assert_eq!(
                calendarmodel, GREGORIAN_CALENDAR,
                "Date should always use the Gregorian calendar model"
            );
        } else {
            panic!("expected a Time StatementValue");
        }
    }

    // ── Reference constructors ────────────────────────────────────────────────

    #[test]
    fn test_reference_none_gives_no_group() {
        // A none() reference carries no data, so it cannot produce a reference group.
        assert!(
            Reference::none().as_ref_group().is_none(),
            "Reference::none() should produce no reference group"
        );
    }

    #[test]
    fn test_reference_none_is_never_equivalent() {
        // A none() reference is not equivalent to anything.
        let group = Reference::prop("P675", "BookID").as_ref_group().unwrap();
        assert!(
            !Reference::none().is_equivalent(&group),
            "Reference::none() should never be equivalent to any reference group"
        );
    }

    #[test]
    fn test_reference_prop_produces_group() {
        let r = Reference::prop("P675", "BookID");
        let group = r
            .as_ref_group()
            .expect("prop reference should produce a group");
        // Must contain the stated property …
        let has_p675 = group.parts().iter().any(|pv| pv.property().id() == "P675");
        // … and a P813 "retrieved" date.
        let has_p813 = group.parts().iter().any(|pv| pv.property().id() == "P813");
        assert!(has_p675, "group should contain the stated property P675");
        assert!(has_p813, "group should contain P813 (retrieved date)");
    }

    #[test]
    fn test_reference_prop_group_has_correct_value() {
        let r = Reference::prop("P675", "BookID");
        let group = r.as_ref_group().unwrap();
        let p675_value = group
            .parts()
            .iter()
            .find(|pv| pv.property().id() == "P675")
            .and_then(|pv| match pv.value() {
                StatementValue::Value(StatementValueContent::String(s)) => Some(s.clone()),
                _ => None,
            });
        assert_eq!(
            p675_value,
            Some("BookID".to_string()),
            "P675 part should carry the value 'BookID'"
        );
    }

    // ── Reference::is_equivalent ─────────────────────────────────────────────

    #[test]
    fn test_is_equivalent_round_trip() {
        // A reference built via prop() must be equivalent to the group it produces.
        let r = Reference::prop("P8383", "12345");
        let group = r.as_ref_group().unwrap();
        assert!(
            r.is_equivalent(&group),
            "prop reference must be equivalent to its own as_ref_group output"
        );
    }

    #[test]
    fn test_is_equivalent_wrong_value() {
        let r = Reference::prop("P675", "BookID");
        let other_group = Reference::prop("P675", "OtherID").as_ref_group().unwrap();
        assert!(
            !r.is_equivalent(&other_group),
            "reference should not be equivalent when the value differs"
        );
    }

    #[test]
    fn test_is_equivalent_wrong_property() {
        let r = Reference::prop("P675", "BookID");
        let other_group = Reference::prop("P957", "BookID").as_ref_group().unwrap();
        assert!(
            !r.is_equivalent(&other_group),
            "reference should not be equivalent when the property differs"
        );
    }

    // ── DataValue: Hash + Eq consistency ─────────────────────────────────────

    #[test]
    fn test_data_value_equality() {
        assert_eq!(
            DataValue::String("x".to_string()),
            DataValue::String("x".to_string())
        );
        assert_ne!(
            DataValue::String("x".to_string()),
            DataValue::String("y".to_string())
        );
        assert_ne!(
            DataValue::String("x".to_string()),
            DataValue::Entity("x".to_string()),
            "String and Entity variants with same payload must be distinct"
        );
    }

    // ── compare ──────────────────────────────────────────────────────────────

    fn quantity(amount: &str, unit: &str) -> StatementValue {
        StatementValue::Value(StatementValueContent::Quantity {
            amount: amount.to_string(),
            unit: unit.to_string(),
        })
    }

    fn time(time: &str, precision: TimePrecision) -> StatementValue {
        StatementValue::Value(StatementValueContent::Time {
            time: time.to_string(),
            precision,
            calendarmodel: GREGORIAN_CALENDAR.to_string(),
        })
    }

    fn year(year: &str) -> DataValue {
        DataValue::Date {
            time: format!("+{year}-01-01T00:00:00Z"),
            precision: TimePrecision::Year,
        }
    }

    #[test]
    fn test_compare_quantity_by_number() {
        let pages = DataValue::Quantity(515);
        assert_eq!(pages.compare(&quantity("+515", "1")), ValueMatch::Same);
        assert_eq!(pages.compare(&quantity("+0515", "1")), ValueMatch::Same);
        assert_eq!(pages.compare(&quantity("+528", "1")), ValueMatch::Different);
        let metre = "http://www.wikidata.org/entity/Q11573";
        assert_eq!(
            pages.compare(&quantity("+515", metre)),
            ValueMatch::Different
        );
    }

    #[test]
    fn test_compare_date_at_precision() {
        let y2014 = year("2014");
        for existing in ["+2014-00-00T00:00:00Z", "+2014-01-01T00:00:00Z"] {
            let existing = time(existing, TimePrecision::Year);
            assert_eq!(y2014.compare(&existing), ValueMatch::Same);
        }
        let month = time("+2014-10-00T00:00:00Z", TimePrecision::Month);
        assert_eq!(y2014.compare(&month), ValueMatch::MorePrecise);
        let other_year = time("+2015-00-00T00:00:00Z", TimePrecision::Year);
        assert_eq!(y2014.compare(&other_year), ValueMatch::Different);
        let decade = time("+2010-00-00T00:00:00Z", TimePrecision::Decade);
        assert_eq!(y2014.compare(&decade), ValueMatch::Different);
        let bce = time("-2014-00-00T00:00:00Z", TimePrecision::Year);
        assert_eq!(y2014.compare(&bce), ValueMatch::Different);
    }

    #[test]
    fn test_compare_date_requires_gregorian() {
        let julian = StatementValue::Value(StatementValueContent::Time {
            time: "+2014-00-00T00:00:00Z".to_string(),
            precision: TimePrecision::Year,
            calendarmodel: JULIAN_CALENDAR.to_string(),
        });
        assert_eq!(year("2014").compare(&julian), ValueMatch::Different);
    }

    #[test]
    fn test_compare_other_values_by_equality() {
        let s = DataValue::String("84-226-4891-1".to_string());
        assert_eq!(s.compare(&s.as_statement_value()), ValueMatch::Same);
        let other = DataValue::String("x".to_string()).as_statement_value();
        assert_eq!(s.compare(&other), ValueMatch::Different);
        assert_eq!(s.compare(&StatementValue::NoValue), ValueMatch::Different);
    }

    #[test]
    fn test_data_value_quantity_sign() {
        // Negative quantities must round-trip correctly through as_statement_value
        let sv = DataValue::Quantity(-7).as_statement_value();
        assert!(
            matches!(
                sv,
                StatementValue::Value(StatementValueContent::Quantity { amount, .. })
                    if amount == "-7"
            ),
            "Negative quantity must produce '-7' as amount string"
        );
    }

    #[test]
    fn test_reference_url_produces_group_with_p854() {
        let r = Reference::_url("https://example.com/page");
        let group = r
            .as_ref_group()
            .expect("URL reference should produce a group");
        let has_p854 = group.parts().iter().any(|pv| pv.property().id() == "P854");
        assert!(has_p854, "URL reference group should contain P854");
        let has_p813 = group.parts().iter().any(|pv| pv.property().id() == "P813");
        assert!(
            has_p813,
            "URL reference group should contain P813 (retrieved date)"
        );
    }

    #[test]
    fn test_reference_url_is_equivalent_to_own_group() {
        let r = Reference::_url("https://example.com/page");
        let group = r.as_ref_group().unwrap();
        assert!(r.is_equivalent(&group));
    }

    #[test]
    fn test_reference_url_not_equivalent_to_different_url() {
        let r = Reference::_url("https://example.com/page");
        let other_group = Reference::_url("https://other.com/page")
            .as_ref_group()
            .unwrap();
        assert!(!r.is_equivalent(&other_group));
    }

    #[test]
    fn test_reference_default_is_none() {
        let r = Reference::default();
        assert!(r.as_ref_group().is_none());
    }
}
