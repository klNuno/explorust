//! Settings pages from Windows' own list: `%WINDIR%\ImmersiveControlPanel\Settings\AllSystemSettings_*.xml`,
//! one `<SearchableContent>` per setting with its deep link, its name (an indirect string resolved
//! in the UI language) and keywords. The parse is plain text; resolving needs Windows.

/// One setting as the file gives it: strings may be indirect (`@dll,-id` or `@{pri?ms-resource:}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawSetting {
    pub name: String,
    pub link: String,
    pub keywords: Vec<String>,
}

/// The text of the first `<tag>...</tag>` in `s`, entities decoded.
fn tag<'a>(s: &'a str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let a = s.find(&open)? + open.len();
    let b = a + s[a..].find(&close)?;
    Some(unescape(s[a..b].trim()))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Every setting with a name and a page: Windows 11 names the page (`<Node Type="Page">`) where
/// Windows 10 gave a `<DeepLink>`; the page becomes an `ms-settings:` link (`page_link`).
pub fn parse(xml: &str) -> Vec<RawSetting> {
    let mut out = Vec::new();
    for block in xml.split("<SearchableContent").skip(1) {
        let block = block.split("</SearchableContent>").next().unwrap_or(block);
        let link = tag(block, "DeepLink").filter(|l| l.starts_with("ms-settings:")).or_else(|| page_of(block).map(|p| page_link(&p)));
        let Some(link) = link else { continue };
        let Some(name) = tag(block, "Description").or_else(|| tag(block, "Name")).filter(|n| !n.is_empty()) else { continue };
        let mut keywords = Vec::new();
        for k in ["HighKeywords", "Keywords", "LowKeywords"] {
            if let Some(v) = tag(block, k).filter(|v| !v.is_empty()) {
                keywords.push(v);
            }
        }
        out.push(RawSetting { name, link, keywords });
    }
    out
}

/// The first `<Node Type="Page">` of a setting.
fn page_of(block: &str) -> Option<String> {
    let open = "<Node Type=\"Page\">";
    let a = block.find(open)? + open.len();
    let b = a + block[a..].find('<')?;
    Some(block[a..b].trim().to_string()).filter(|p| !p.is_empty())
}

/// The `ms-settings:` URI of a page id: the id without `SettingsPage`, lowercased
/// (`SettingsPageDisplay` is `ms-settings:display`), with the pages whose URI differs.
pub fn page_link(page: &str) -> String {
    const KNOWN: &[(&str, &str)] = &[
        ("SettingsPagePowerAndBattery", "ms-settings:powersleep"),
        ("SettingsPageSearchIndex", "ms-settings:search-moredetails"),
        ("SettingsPageRationalizedSearchSettings", "ms-settings:search"),
        ("SettingsPageNetworkStatus", "ms-settings:network-status"),
        ("SettingsPagePersonalizationBackground", "ms-settings:personalization-background"),
        ("SettingsPageAppsFeatures", "ms-settings:appsfeatures"),
        ("SettingsPageAppsDefaults", "ms-settings:defaultapps"),
        ("SettingsPageSystemAbout", "ms-settings:about"),
        ("SettingsPageAppsSound", "ms-settings:sound"),
    ];
    if let Some((_, l)) = KNOWN.iter().find(|(p, _)| p.eq_ignore_ascii_case(page)) {
        return l.to_string();
    }
    let rest = page.strip_prefix("SettingsPage").unwrap_or(page);
    format!("ms-settings:{}", rest.to_lowercase())
}

/// A resolved keyword string is a `;` separated list.
pub fn split_keywords(s: &str) -> Vec<String> {
    s.split(';').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_searchable_content() {
        let xml = r#"<?xml version="1.0"?><PCSettings>
<SearchableContent xmlns="http://schemas.microsoft.com/Search/2013/SettingContent">
  <ApplicationInformation><AppID>windows.immersivecontrolpanel</AppID><DeepLink>ms-settings:display</DeepLink></ApplicationInformation>
  <SettingIdentity><PageID>SettingsPageDisplay</PageID></SettingIdentity>
  <SettingInformation><Description>@%windir%\system32\SettingsHandlers_Display.dll,-1001</Description>
  <HighKeywords>@%windir%\system32\SettingsHandlers_Display.dll,-1002</HighKeywords></SettingInformation>
</SearchableContent>
<SearchableContent><ApplicationInformation><DeepLink>%windir%\system32\control.exe</DeepLink></ApplicationInformation>
<SettingInformation><Description>Old</Description></SettingInformation></SearchableContent>
<SearchableContent><ApplicationInformation><DeepLink>ms-settings:sound</DeepLink></ApplicationInformation>
<SettingInformation><Name>Sound &amp; volume</Name></SettingInformation></SearchableContent>
</PCSettings>"#;
        let s = parse(xml);
        assert_eq!(s.len(), 2);
        let w11 = r#"<SearchableContent IncludeWithFeature="X"><Filename>AAA_Display</Filename>
<SettingIdentity><SettingPaths><Path><Node Type="Page">SettingsPageDisplay</Node><Node Type="Group">G</Node></Path></SettingPaths></SettingIdentity>
<SettingInformation><Description>@{windows?ms-resource://Windows.UI.SettingsAppThreshold/SearchResources/Display/Description}</Description></SettingInformation></SearchableContent>"#;
        let s2 = parse(w11);
        assert_eq!(s2[0].link, "ms-settings:display");
        assert_eq!(page_link("SettingsPagePowerAndBattery"), "ms-settings:powersleep");
        assert_eq!(s[0].link, "ms-settings:display");
        assert_eq!(s[0].name, "@%windir%\\system32\\SettingsHandlers_Display.dll,-1001");
        assert_eq!(s[0].keywords.len(), 1);
        assert_eq!(s[1].name, "Sound & volume");
        assert_eq!(split_keywords("a; b;;c "), vec!["a", "b", "c"]);
    }
}
