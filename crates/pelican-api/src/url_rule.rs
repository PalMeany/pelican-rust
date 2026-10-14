//! Laravel `url` rule: `Str::isUrl` (Illuminate/Support/Str.php), pattern derived from
//! Symfony's UrlValidator. Generated from the PHP source: extended-mode whitespace and
//! comments stripped; possessive `++` relaxed to `+` (no following token overlaps the
//! quantified classes, so the matched language is unchanged).

use std::sync::LazyLock;

static URL_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        "(?i)^",
        r#"(aaa|aaas|about|acap|acct|acd|acr|adiumxtra|adt|afp|afs|aim|amss|android|appdata|apt|ark|attachment|"#,
        r#"aw|barion|beshare|bitcoin|bitcoincash|blob|bolo|browserext|calculator|callto|cap|cast|casts|chrome|c"#,
        r#"hrome-extension|cid|coap|coap\+tcp|coap\+ws|coaps|coaps\+tcp|coaps\+ws|com-eventbrite-attendee|conte"#,
        r#"nt|conti|crid|cvs|dab|data|dav|diaspora|dict|did|dis|dlna-playcontainer|dlna-playsingle|dns|dntp|dpp"#,
        r#"|drm|drop|dtn|dvb|ed2k|elsi|example|facetime|fax|feed|feedready|file|filesystem|finger|first-run-pen"#,
        r#"-experience|fish|fm|ftp|fuchsia-pkg|geo|gg|git|gizmoproject|go|gopher|graph|gtalk|h323|ham|hcap|hcp|"#,
        r#"http|https|hxxp|hxxps|hydrazone|iax|icap|icon|im|imap|info|iotdisco|ipn|ipp|ipps|irc|irc6|ircs|iris|"#,
        r#"iris\.beep|iris\.lwz|iris\.xpc|iris\.xpcs|isostore|itms|jabber|jar|jms|keyparc|lastfm|ldap|ldaps|lea"#,
        r#"ptofrogans|lorawan|lvlt|magnet|mailserver|mailto|maps|market|message|mid|mms|modem|mongodb|moz|ms-ac"#,
        r#"cess|ms-browser-extension|ms-calculator|ms-drive-to|ms-enrollment|ms-excel|ms-eyecontrolspeech|ms-ga"#,
        r#"mebarservices|ms-gamingoverlay|ms-getoffice|ms-help|ms-infopath|ms-inputapp|ms-lockscreencomponent-c"#,
        r#"onfig|ms-media-stream-id|ms-mixedrealitycapture|ms-mobileplans|ms-officeapp|ms-people|ms-project|ms-"#,
        r#"powerpoint|ms-publisher|ms-restoretabcompanion|ms-screenclip|ms-screensketch|ms-search|ms-search-rep"#,
        r#"air|ms-secondary-screen-controller|ms-secondary-screen-setup|ms-settings|ms-settings-airplanemode|ms"#,
        r#"-settings-bluetooth|ms-settings-camera|ms-settings-cellular|ms-settings-cloudstorage|ms-settings-con"#,
        r#"nectabledevices|ms-settings-displays-topology|ms-settings-emailandaccounts|ms-settings-language|ms-s"#,
        r#"ettings-location|ms-settings-lock|ms-settings-nfctransactions|ms-settings-notifications|ms-settings-"#,
        r#"power|ms-settings-privacy|ms-settings-proximity|ms-settings-screenrotation|ms-settings-wifi|ms-setti"#,
        r#"ngs-workplace|ms-spd|ms-sttoverlay|ms-transit-to|ms-useractivityset|ms-virtualtouchpad|ms-visio|ms-w"#,
        r#"alk-to|ms-whiteboard|ms-whiteboard-cmd|ms-word|msnim|msrp|msrps|mss|mtqp|mumble|mupdate|mvn|news|nfs"#,
        r#"|ni|nih|nntp|notes|ocf|oid|onenote|onenote-cmd|opaquelocktoken|openpgp4fpr|pack|palm|paparazzi|payto"#,
        r#"|pkcs11|platform|pop|pres|prospero|proxy|pwid|psyc|pttp|qb|query|redis|rediss|reload|res|resource|rm"#,
        r#"i|rsync|rtmfp|rtmp|rtsp|rtsps|rtspu|s3|secondlife|service|session|sftp|sgn|shttp|sieve|simpleledger|"#,
        r#"sip|sips|skype|smb|sms|smtp|snews|snmp|soap\.beep|soap\.beeps|soldat|spiffe|spotify|ssh|steam|stun|s"#,
        r#"tuns|submit|svn|tag|teamspeak|tel|teliaeid|telnet|tftp|tg|things|thismessage|tip|tn3270|tool|ts3serv"#,
        r#"er|turn|turns|tv|udp|unreal|urn|ut2004|v-event|vemmi|ventrilo|videotex|vnc|view-source|wais|webcal|w"#,
        r#"pid|ws|wss|wtai|wyciwyg|xcon|xcon-userid|xfire|xmlrpc\.beep|xmlrpc\.beeps|xmpp|xri|ymsgr|z39\.50|z39"#,
        r#"\.50r|z39\.50s)://(((?:[_\.\pL\pN-]|%[0-9A-Fa-f]{2})+:)?((?:[_\.\pL\pN-]|%[0-9A-Fa-f]{2})+)@)?((?:(?"#,
        r#":(?:[\pL\pN\pS\pM\-_]+\.)+(?:(?:xn--[a-z0-9-]+)|(?:[\pL\pN\pM]+)))|[a-z0-9\-_]+)\.?|\d{1,3}\.\d{1,3}"#,
        r#"\.\d{1,3}\.\d{1,3}|\[(?:(?:(?:(?:(?:(?:(?:[0-9a-f]{1,4})):){6})(?:(?:(?:(?:(?:[0-9a-f]{1,4})):(?:(?:"#,
        r#"[0-9a-f]{1,4})))|(?:(?:(?:(?:(?:25[0-5]|(?:[1-9]|1[0-9]|2[0-4])?[0-9]))\.){3}(?:(?:25[0-5]|(?:[1-9]|"#,
        r#"1[0-9]|2[0-4])?[0-9])))))))|(?:(?:::(?:(?:(?:[0-9a-f]{1,4})):){5})(?:(?:(?:(?:(?:[0-9a-f]{1,4})):(?:"#,
        r#"(?:[0-9a-f]{1,4})))|(?:(?:(?:(?:(?:25[0-5]|(?:[1-9]|1[0-9]|2[0-4])?[0-9]))\.){3}(?:(?:25[0-5]|(?:[1-"#,
        r#"9]|1[0-9]|2[0-4])?[0-9])))))))|(?:(?:(?:(?:(?:[0-9a-f]{1,4})))?::(?:(?:(?:[0-9a-f]{1,4})):){4})(?:(?"#,
        r#":(?:(?:(?:[0-9a-f]{1,4})):(?:(?:[0-9a-f]{1,4})))|(?:(?:(?:(?:(?:25[0-5]|(?:[1-9]|1[0-9]|2[0-4])?[0-9"#,
        r#"]))\.){3}(?:(?:25[0-5]|(?:[1-9]|1[0-9]|2[0-4])?[0-9])))))))|(?:(?:(?:(?:(?:(?:[0-9a-f]{1,4})):){0,1}"#,
        r#"(?:(?:[0-9a-f]{1,4})))?::(?:(?:(?:[0-9a-f]{1,4})):){3})(?:(?:(?:(?:(?:[0-9a-f]{1,4})):(?:(?:[0-9a-f]"#,
        r#"{1,4})))|(?:(?:(?:(?:(?:25[0-5]|(?:[1-9]|1[0-9]|2[0-4])?[0-9]))\.){3}(?:(?:25[0-5]|(?:[1-9]|1[0-9]|2"#,
        r#"[0-4])?[0-9])))))))|(?:(?:(?:(?:(?:(?:[0-9a-f]{1,4})):){0,2}(?:(?:[0-9a-f]{1,4})))?::(?:(?:(?:[0-9a-"#,
        r#"f]{1,4})):){2})(?:(?:(?:(?:(?:[0-9a-f]{1,4})):(?:(?:[0-9a-f]{1,4})))|(?:(?:(?:(?:(?:25[0-5]|(?:[1-9]"#,
        r#"|1[0-9]|2[0-4])?[0-9]))\.){3}(?:(?:25[0-5]|(?:[1-9]|1[0-9]|2[0-4])?[0-9])))))))|(?:(?:(?:(?:(?:(?:[0"#,
        r#"-9a-f]{1,4})):){0,3}(?:(?:[0-9a-f]{1,4})))?::(?:(?:[0-9a-f]{1,4})):)(?:(?:(?:(?:(?:[0-9a-f]{1,4})):("#,
        r#"?:(?:[0-9a-f]{1,4})))|(?:(?:(?:(?:(?:25[0-5]|(?:[1-9]|1[0-9]|2[0-4])?[0-9]))\.){3}(?:(?:25[0-5]|(?:["#,
        r#"1-9]|1[0-9]|2[0-4])?[0-9])))))))|(?:(?:(?:(?:(?:(?:[0-9a-f]{1,4})):){0,4}(?:(?:[0-9a-f]{1,4})))?::)("#,
        r#"?:(?:(?:(?:(?:[0-9a-f]{1,4})):(?:(?:[0-9a-f]{1,4})))|(?:(?:(?:(?:(?:25[0-5]|(?:[1-9]|1[0-9]|2[0-4])?"#,
        r#"[0-9]))\.){3}(?:(?:25[0-5]|(?:[1-9]|1[0-9]|2[0-4])?[0-9])))))))|(?:(?:(?:(?:(?:(?:[0-9a-f]{1,4})):){"#,
        r#"0,5}(?:(?:[0-9a-f]{1,4})))?::)(?:(?:[0-9a-f]{1,4})))|(?:(?:(?:(?:(?:(?:[0-9a-f]{1,4})):){0,6}(?:(?:["#,
        r#"0-9a-f]{1,4})))?::))))\])(:[0-9]+)?(?:/(?:[\pL\pN\-._~!$&'()*+,;=:@]|%[0-9A-Fa-f]{2})*)*(?:\?(?:[\pL"#,
        r#"\pN\-._~!$&'\[\]()*+,;=:@/?]|%[0-9A-Fa-f]{2})*)?(?:\#(?:[\pL\pN\-._~!$&'()*+,;=:@/?]|%[0-9A-Fa-f]{2}"#,
        r#")*)?"#,
        "$"
    ))
    .expect("valid url regex")
});

/// `Str::isUrl($value)` with the default protocol list.
pub fn is_url(s: &str) -> bool {
    URL_RE.is_match(s)
}

#[cfg(test)]
mod tests {
    use super::is_url;

    #[test]
    fn urls() {
        for ok in [
            "http://example.com",
            "https://a.b.c/x?y=1#z",
            "ftp://user:pw@host:21/p",
            "http://[::1]:8080/",
            "http://127.0.0.1/a%20b",
            "https://exämple.de/ü",
            "http://localhost",
            "steam://run/730",
        ] {
            assert!(is_url(ok), "{ok}");
        }
        for bad in ["example.com", "http://", "http://a b", "javascript:alert(1)", "http://exa mple.com", "nope://x"] {
            assert!(!is_url(bad), "{bad}");
        }
    }
}
