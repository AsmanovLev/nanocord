use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum AppLanguage {
    Ru,
    En,
}

impl AppLanguage {
    pub fn code(&self) -> &'static str {
        match self {
            AppLanguage::Ru => "ru",
            AppLanguage::En => "en",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code.trim().to_lowercase().as_str() {
            "ru" => Some(AppLanguage::Ru),
            "en" => Some(AppLanguage::En),
            _ => None,
        }
    }
}

pub struct Localization {
    // string name -> (language code -> translated text)
    entries: HashMap<String, HashMap<String, String>>,
}

impl Localization {
    /// Разбор встроенного strings.xml файла ресурсов
    pub fn load_from_xml(xml: &str) -> Self {
        let mut entries: HashMap<String, HashMap<String, String>> = HashMap::new();

        let mut rest = xml;
        while let Some(start_pos) = rest.find("<string ") {
            let after_tag = &rest[start_pos + 8..];
            let end_pos = match after_tag.find("</string>") {
                Some(p) => p,
                None => break,
            };
            let block = &after_tag[..end_pos];
            rest = &after_tag[end_pos + 9..];

            // Извлечение атрибута name="..."
            let name = if let Some(n_start) = block.find("name=\"") {
                let n_after = &block[n_start + 6..];
                if let Some(n_end) = n_after.find('\"') {
                    n_after[..n_end].trim()
                } else {
                    continue;
                }
            } else {
                continue;
            };

            // Извлечение содержимого внутри тега <string ...>...</string>
            let inner = match block.find('>') {
                Some(pos) => &block[pos + 1..],
                None => continue,
            };

            let mut lang_map = HashMap::new();
            let mut scan = inner;

            // Поиск дочерних языковых тегов: <ru>...</ru>, <en>...</en>, <de>...</de> и т.д.
            while let Some(open_pos) = scan.find('<') {
                scan = &scan[open_pos + 1..];
                let tag_name_end = match scan.find('>') {
                    Some(p) => p,
                    None => break,
                };
                let tag_name = scan[..tag_name_end].trim();
                scan = &scan[tag_name_end + 1..];

                // Игнорируем закрывающие теги если встретились случайно
                if tag_name.starts_with('/') {
                    continue;
                }

                let close_tag = format!("</{}>", tag_name);
                let val_end = match scan.find(&close_tag) {
                    Some(p) => p,
                    None => break,
                };
                let raw_val = &scan[..val_end];
                scan = &scan[val_end + close_tag.len()..];

                let clean_val = unescape_xml(raw_val.trim());
                lang_map.insert(tag_name.to_lowercase(), clean_val);
            }

            entries.insert(name.to_string(), lang_map);
        }

        Self { entries }
    }

    /// Быстрый поиск строки с каскадным fallback: Выбранный язык -> EN -> Имя ключа
    pub fn get<'a>(&'a self, key: &'a str, lang: AppLanguage) -> &'a str {
        if let Some(lang_map) = self.entries.get(key) {
            if let Some(val) = lang_map.get(lang.code()) {
                return val.as_str();
            }
            if let Some(val) = lang_map.get("en") {
                return val.as_str();
            }
            if let Some(val) = lang_map.get("ru") {
                return val.as_str();
            }
        }
        key
    }
}

/// Раскодирование стандартных XML-сущностей
fn unescape_xml(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("\\n", "\n")
}

static I18N: OnceLock<Localization> = OnceLock::new();

/// Получение синглтона локализации со встроенным XML
pub fn get_i18n() -> &'static Localization {
    I18N.get_or_init(|| {
        let xml_content = include_str!("../assets/strings.xml");
        Localization::load_from_xml(xml_content)
    })
}

/// Глобальная функция перевода: t("nav_lobby", lang) -> &'static str
#[inline]
pub fn t(key: &'static str, lang: AppLanguage) -> &'static str {
    get_i18n().get(key, lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_strings_xml() {
        let loc = get_i18n();
        assert_eq!(loc.get("app_title", AppLanguage::Ru), "NANOCORD");
        assert_eq!(loc.get("app_title", AppLanguage::En), "NANOCORD");
        assert_eq!(loc.get("nav_lobby", AppLanguage::Ru), "Комната");
        assert_eq!(loc.get("nav_lobby", AppLanguage::En), "Lobby");
        assert_eq!(loc.get("nav_call_and_stream", AppLanguage::Ru), "Звонок и Стрим");
        assert_eq!(loc.get("nav_call_and_stream", AppLanguage::En), "Call & Stream");
        assert_eq!(loc.get("dock_end_call_btn", AppLanguage::Ru), "Завершить звонок");
        assert_eq!(loc.get("dock_end_call_btn", AppLanguage::En), "End Call");
    }

    #[test]
    fn test_fallback_behavior() {
        let xml = r#"
            <resources>
                <string name="test_key">
                    <en>English Only</en>
                </string>
            </resources>
        "#;
        let loc = Localization::load_from_xml(xml);
        assert_eq!(loc.get("test_key", AppLanguage::Ru), "English Only");
        assert_eq!(loc.get("missing_key", AppLanguage::Ru), "missing_key");
    }
}
