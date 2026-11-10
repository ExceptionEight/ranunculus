//! Player commands said plainly — «пауза», «дальше», «выключи музыку» — go straight to the player, past the model.
use regex::Regex;
use std::sync::OnceLock;

const COMMANDS: [(&str, &str); 10] = [
    ("stop", "(стоп|хватит|выключи|выключай|выключи музыку|выруби( музыку)?|останови( музыку)?|отключи музыку|тишина)"),
    ("pause", "(пауза|паузу|на паузу|поставь на паузу|поставь паузу|подожди)"),
    // «Продолжить.», sent to the model, may get «Продолжаю» with no player_control call
    ("resume", "(продолж(и|ить|ай|аем|им)( играть)?|возобнови(ть)?|играй( дальше)?|сыграй дальше|сними( с)? пауз[уы]|\
                убери паузу|включи (обратно|снова)|включай|верни музыку|поехали)"),
    ("next", "(дальше|следующ(ая|ую|ий|ее)( песня| песню| трек)?|переключи|другую( песню)?|пропусти|скипни|не то)"),
    ("previous", "(назад|предыдущ(ая|ую|ий|ее)( песня| песню| трек)?|верни предыдущ(ую|ий))"),
    ("restart", "(((включи|поставь|начни|играй|сыграй|запусти)( ее| его| эту)? )?(сначала|с начала|заново|с самого начала)|\
                 перемотай (в|на) начало|в начало)"),
    // the repeat: of the track on now, not of the queue
    ("repeat", "(((поставь|включи|сделай)( ее| его| эту)? )?на повтор|(включи|поставь) повтор|повтор|повторяй( ее| его| эту)?|\
                зацикли( ее| его| эту)?)"),
    ("repeat_off", "((убери|сними|выключи|отключи|отмени|выруби)( с)? повтор(а)?|не повторяй|хватит повторять|без повтора|\
                    повтор (выключи|убери|не надо))"),
    // a number («на минуту вперёд») goes to the model: player_control seek
    ("back", "((перемотай|отмотай|промотай|мотни|откати)( немного| чуть| чуть чуть| слегка)? назад)"),
    ("forward", "((перемотай|промотай|мотни)( немного| чуть| чуть чуть| слегка)? вперед)"),
];
/// What the history keeps as the answer (the player obeys silently: only a failure is said aloud).
pub fn say(action: &str) -> &'static str {
    match action {
        "stop" => "Выключила.",
        "pause" => "Пауза.",
        "resume" => "Продолжаю.",
        "next" => "Следующая.",
        "previous" => "Предыдущая.",
        "restart" => "Сначала.",
        "repeat" => "На повторе.",
        "repeat_off" => "Повтор выключен.",
        "back" => "Назад на 15 секунд.",
        "forward" => "Вперёд на 15 секунд.",
        _ => "",
    }
}

/// «Пауза», «дальше», «выключи музыку»…: a player command said plainly → the action, else None.
pub fn spoken_command(text: &str) -> Option<&'static str> {
    static RES: OnceLock<(Vec<(&'static str, Regex)>, Regex, Regex, Regex)> = OnceLock::new();
    let (cmds, junk, filler, whole_off) = RES.get_or_init(|| {
        (
            COMMANDS.iter().map(|(a, p)| (*a, Regex::new(&format!("^(?:{p})$")).unwrap())).collect(),
            Regex::new("[^а-яё ]").unwrap(),
            Regex::new(r"\b(пожалуйста|ну|давай|а|музыку|музыка|песню|трек)\b").unwrap(),
            Regex::new(r"^\s*(выключи музыку|отключи музыку)\s*$").unwrap(),
        )
    });
    let t = junk.replace_all(&text.to_lowercase().replace('ё', "е"), " ").to_string();
    let t = if whole_off.is_match(&t) { t } else { filler.replace_all(&t, " ").to_string() };
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    // «Фото пауза»: the wake word, garbled, left in front of the command — the rest is a command alone
    let mut cands = vec![t.clone()];
    if let Some((first, rest)) = t.split_once(' ') {
        if !["не", "нет", "без"].contains(&first) {
            cands.push(rest.to_string());
        }
    }
    for c in &cands {
        for (action, re) in cmds {
            if re.is_match(c) {
                return Some(action);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_a_command_and_what_is_not() {
        for (t, want) in [
            ("Пауза.", Some("pause")),
            ("Поставь на паузу, пожалуйста.", Some("pause")),
            ("Фото пауза", Some("pause")),
            ("Дальше!", Some("next")),
            ("Следующую песню.", Some("next")),
            ("Выключи музыку.", Some("stop")),
            ("Выключи свет.", None),
            ("Не пауза", None),
            ("Продолжи.", Some("resume")),
            ("Продолжить", Some("resume")),
            ("Включи обратно", Some("resume")),
            ("Назад", Some("previous")),
            ("Верни предыдущую", Some("previous")),
            ("Ну давай дальше", Some("next")),
            ("Хватит", Some("stop")),
            ("Стоп.", Some("stop")),
            ("Пауза в работе была", None),
            ("Мразота стоп", Some("stop")),
            ("нет дальше", None),
            ("Какая погода?", None),
            ("Подожди", Some("pause")),
            ("Скипни", Some("next")),
            ("отключи музыку", Some("stop")),
            ("выруби музыку", Some("stop")),
            ("играй дальше", Some("resume")),
            ("Сними с паузы", Some("resume")),
            ("поехали", Some("resume")),
            ("не то", Some("next")),
            ("Включи сначала.", Some("restart")),
            ("Включи её сначала", Some("restart")),
            ("Давай с самого начала", Some("restart")),
            ("Заново.", Some("restart")),
            ("Перемотай в начало", Some("restart")),
            ("Поставь эту песню сначала", Some("restart")),
            ("Сначала скажи погоду", None),
            ("Начни сначала", Some("restart")),
            ("Поставь на повтор.", Some("repeat")),
            ("Поставь эту песню на повтор", Some("repeat")),
            ("Повтор", Some("repeat")),
            ("Повторяй её", Some("repeat")),
            ("Зацикли", Some("repeat")),
            ("Повтори", None),
            ("Повтори, что ты сказала", None),
            ("Убери повтор", Some("repeat_off")),
            ("Выключи повтор.", Some("repeat_off")),
            ("Сними с повтора", Some("repeat_off")),
            ("Не повторяй", Some("repeat_off")),
            ("Перемотай назад", Some("back")),
            ("Отмотай чуть-чуть назад", Some("back")),
            ("Перемотай вперёд", Some("forward")),
            ("Промотай немного вперёд", Some("forward")),
            ("Перемотай на 30 секунд назад", None),
            ("Перемотай на минуту вперёд", None),
            ("Вперёд", None),
        ] {
            assert_eq!(spoken_command(t), want, "{t}");
        }
    }
}
