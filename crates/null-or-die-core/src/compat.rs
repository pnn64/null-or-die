pub fn guess_paradigm(
    sync_bias_ms: f64,
    tolerance: f64,
    consider_null: bool,
    consider_p9ms: bool,
    short_paradigm: bool,
) -> &'static str {
    let near_null = sync_bias_ms > -tolerance && sync_bias_ms < tolerance;
    let near_p9 = sync_bias_ms > (9.0 - tolerance) && sync_bias_ms < (9.0 + tolerance);
    if consider_null && near_null {
        if short_paradigm {
            "null"
        } else {
            "probably null"
        }
    } else if consider_p9ms && near_p9 {
        if short_paradigm {
            "+9ms"
        } else {
            "probably +9ms"
        }
    } else if short_paradigm {
        "????"
    } else {
        "unclear paradigm"
    }
}

pub fn slot_abbreviation(
    steps_type: &str,
    chart_slot: &str,
    chart_index: usize,
    paradigm: &str,
) -> String {
    if paradigm == "+9ms" {
        let style = match steps_type {
            "dance-single" => "S",
            "dance-double" => "D",
            _ => "?",
        };
        let slot = match chart_slot {
            "Challenge" => "X",
            "Hard" => "H",
            "Medium" => "M",
            "Easy" => "E",
            "Beginner" => "N",
            "Edit" => ".",
            _ => "?",
        };
        if chart_slot == "Edit" {
            format!("{style}{slot}{chart_index}")
        } else {
            format!("{style}{slot}")
        }
    } else {
        let style = match steps_type {
            "dance-single" => "SP",
            "dance-double" => "DP",
            _ => "?",
        };
        let slot = match chart_slot {
            "Challenge" => "C",
            "Hard" => "E",
            "Medium" => "D",
            "Easy" => "B",
            "Beginner" => "b",
            "Edit" => "X",
            _ => "?",
        };
        if chart_slot == "Edit" {
            format!("{slot}{chart_index}{style}")
        } else {
            format!("{slot}{style}")
        }
    }
}

pub fn slot_expansion(abbr: &str) -> Result<(String, String, Option<usize>), String> {
    if abbr.ends_with("SP") || abbr.ends_with("DP") {
        expand_null_slot(abbr)
    } else if abbr.starts_with('S') || abbr.starts_with('D') {
        expand_p9_slot(abbr)
    } else {
        Err(format!("Could not parse slot abbreviation: {abbr}"))
    }
}

fn expand_null_slot(abbr: &str) -> Result<(String, String, Option<usize>), String> {
    if abbr.len() < 3 {
        return Err(format!("Invalid null slot abbreviation: {abbr}"));
    }
    let style = &abbr[abbr.len() - 2..];
    let steps_type = match style {
        "SP" => "dance-single",
        "DP" => "dance-double",
        _ => return Err(format!("Unknown style in abbreviation: {abbr}")),
    };
    let slot = &abbr[0..1];
    let chart_slot = match slot {
        "C" => "Challenge",
        "E" => "Hard",
        "D" => "Medium",
        "B" => "Easy",
        "b" => "Beginner",
        "X" => "Edit",
        _ => return Err(format!("Unknown difficulty in abbreviation: {abbr}")),
    };
    let index = parse_middle_index(abbr, 1, abbr.len() - 2)?;
    Ok((steps_type.to_string(), chart_slot.to_string(), index))
}

fn expand_p9_slot(abbr: &str) -> Result<(String, String, Option<usize>), String> {
    if abbr.len() < 2 {
        return Err(format!("Invalid +9ms slot abbreviation: {abbr}"));
    }
    let style = &abbr[0..1];
    let steps_type = match style {
        "S" => "dance-single",
        "D" => "dance-double",
        _ => return Err(format!("Unknown style in abbreviation: {abbr}")),
    };
    let slot = &abbr[1..2];
    let chart_slot = match slot {
        "X" => "Challenge",
        "H" => "Hard",
        "M" => "Medium",
        "E" => "Easy",
        "N" => "Beginner",
        "." => "Edit",
        _ => return Err(format!("Unknown difficulty in abbreviation: {abbr}")),
    };
    let index = parse_middle_index(abbr, 2, abbr.len())?;
    Ok((steps_type.to_string(), chart_slot.to_string(), index))
}

fn parse_middle_index(abbr: &str, start: usize, end: usize) -> Result<Option<usize>, String> {
    if end <= start {
        return Ok(None);
    }
    let raw = &abbr[start..end];
    if raw.is_empty() {
        Ok(None)
    } else {
        raw.parse::<usize>()
            .map(Some)
            .map_err(|_| format!("Invalid chart index in abbreviation: {abbr}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{guess_paradigm, slot_abbreviation, slot_expansion};

    #[test]
    fn guess_paradigm_matches_python_cases() {
        assert_eq!(guess_paradigm(0.0, 3.0, true, true, true), "null");
        assert_eq!(guess_paradigm(8.9, 3.0, true, true, true), "+9ms");
        assert_eq!(guess_paradigm(20.0, 3.0, true, true, true), "????");
        assert_eq!(
            guess_paradigm(8.9, 3.0, false, true, false),
            "probably +9ms"
        );
        assert_eq!(
            guess_paradigm(0.1, 3.0, false, true, false),
            "unclear paradigm"
        );
    }

    #[test]
    fn slot_roundtrip_for_null_paradigm() {
        let abbr = slot_abbreviation("dance-single", "Challenge", 0, "null");
        let expanded = slot_expansion(&abbr).expect("slot expansion failed");
        assert_eq!(
            expanded,
            ("dance-single".to_string(), "Challenge".to_string(), None)
        );
    }

    #[test]
    fn slot_roundtrip_for_p9ms_edit() {
        let abbr = slot_abbreviation("dance-double", "Edit", 3, "+9ms");
        let expanded = slot_expansion(&abbr).expect("slot expansion failed");
        assert_eq!(
            expanded,
            ("dance-double".to_string(), "Edit".to_string(), Some(3))
        );
    }
}
