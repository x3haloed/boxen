//! The flat primitive form vocabulary supported by ACP elicitation.
use serde_json::{Value, json};
use std::collections::HashSet;

pub struct Form {
    schema: Value,
}

impl Form {
    pub fn parse(schema: &Value) -> Option<Self> {
        let properties = schema["properties"].as_object()?;
        if schema["type"] != "object" || properties.len() > 32 {
            return None;
        }
        for property in properties.values() {
            match property["type"].as_str()? {
                "string" => {
                    if !property["oneOf"].is_null() {
                        choices(&property["oneOf"], true)?;
                    } else if !property["enum"].is_null() {
                        choices(&property["enum"], false)?;
                    }
                }
                "number" | "integer" | "boolean" => {}
                "array" => {
                    if !property["items"]["anyOf"].is_null() {
                        choices(&property["items"]["anyOf"], true)?;
                    } else if property["items"]["type"] == "string" {
                        choices(&property["items"]["enum"], false)?;
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        if !schema["required"].is_null() {
            for field in schema["required"].as_array()? {
                if !properties.contains_key(field.as_str()?) {
                    return None;
                }
            }
        }
        Some(Self {
            schema: schema.clone(),
        })
    }

    pub fn response(&self, reply: &Value) -> Option<Value> {
        let reply = reply.as_object()?;
        let action = reply.get("action")?.as_str()?;
        match action {
            "decline" | "cancel" if reply.len() == 1 => Some(json!({"action":action})),
            "accept" if reply.len() == 2 => {
                let content = reply.get("content")?.as_object()?;
                if let Some(required) = self.schema["required"].as_array()
                    && required
                        .iter()
                        .any(|field| !content.contains_key(field.as_str().unwrap_or("")))
                {
                    return None;
                }
                if !content.iter().all(|(key, value)| {
                    self.schema["properties"]
                        .get(key)
                        .is_some_and(|property| accepts(property, value))
                }) {
                    return None;
                }
                Some(json!({"action":"accept","content":content}))
            }
            _ => None,
        }
    }
}

fn choices(entries: &Value, titled: bool) -> Option<HashSet<&str>> {
    let entries = entries.as_array()?;
    let mut values = HashSet::new();
    for entry in entries {
        let value = if titled {
            entry["title"].as_str()?;
            entry["const"].as_str()?
        } else {
            entry.as_str()?
        };
        if !values.insert(value) {
            return None;
        }
    }
    (!values.is_empty()).then_some(values)
}

fn within(value: f64, min: &Value, max: &Value) -> bool {
    min.as_f64().is_none_or(|min| value >= min) && max.as_f64().is_none_or(|max| value <= max)
}

fn offered(value: &str, entries: &Value, titled: bool) -> bool {
    choices(entries, titled).is_some_and(|options| options.contains(value))
}

fn accepts(property: &Value, value: &Value) -> bool {
    match property["type"].as_str().unwrap_or("") {
        "string" => value.as_str().is_some_and(|value| {
            if !property["oneOf"].is_null() {
                offered(value, &property["oneOf"], true)
            } else if !property["enum"].is_null() {
                offered(value, &property["enum"], false)
            } else {
                within(
                    value.chars().count() as f64,
                    &property["minLength"],
                    &property["maxLength"],
                )
            }
        }),
        "boolean" => value.is_boolean(),
        "number" | "integer" => value.as_f64().is_some_and(|number| {
            (property["type"] == "number" || number.fract() == 0.0)
                && within(number, &property["minimum"], &property["maximum"])
        }),
        "array" => value.as_array().is_some_and(|values| {
            let items = &property["items"];
            let titled = !items["anyOf"].is_null();
            let options = choices(&items[if titled { "anyOf" } else { "enum" }], titled);
            let mut selected = HashSet::new();
            within(
                values.len() as f64,
                &property["minItems"],
                &property["maxItems"],
            ) && options.is_some_and(|options| {
                values.iter().all(|value| {
                    value
                        .as_str()
                        .is_some_and(|value| options.contains(value) && selected.insert(value))
                })
            })
        }),
        _ => false,
    }
}
