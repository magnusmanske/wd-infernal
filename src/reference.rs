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
            DataValue::Quantity(amount) => StatementValueContent::Quantity {
                amount: format!("{amount}"),
                unit: "".to_string(),
            },
        };
        StatementValue::Value(svc)
    }
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

    fn _url(url: &str) -> Self {
        Reference {
            property: None,
            value: None,
            url: Some(url.to_string()),
        }
    }

    pub fn is_equivalent(&self, reference: &wikibase_rest_api::Reference) -> bool {
        if let (Some(property), Some(value)) = (&self.property, &self.value) {
            reference.parts().iter().any(|prop_value| {
                let ref_prop = prop_value.property().id();
                let ref_value = match prop_value.value() {
                    StatementValue::Value(statement_value_content) => statement_value_content,
                    _ => return false,
                };
                let ref_value = match ref_value {
                    StatementValueContent::String(s) => s,
                    _ => return false,
                    // StatementValueContent::Time { time, precision, calendarmodel } => todo!(),
                    // StatementValueContent::Location { latitude, longitude, precision, globe } => todo!(),
                    // StatementValueContent::Quantity { amount, unit } => todo!(),
                    // StatementValueContent::MonolingualText { language, text } => todo!(),
                };
                property == ref_prop && value == ref_value
            })
        } else if let Some(url) = &self.url {
            reference.parts().iter().any(|prop_value| {
                let ref_prop = prop_value.property().id();
                let ref_value = match prop_value.value() {
                    StatementValue::Value(statement_value_content) => statement_value_content,
                    _ => return false,
                };
                let ref_value = match ref_value {
                    StatementValueContent::String(s) => s,
                    _ => return false,
                };
                ref_prop == "P854" && url == ref_value
            })
        } else {
            false
        }
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
