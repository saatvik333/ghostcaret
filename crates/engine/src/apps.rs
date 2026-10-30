//! App categories from built-in tables of Linux app ids (Wayland `app_id`,
//! Flatpak ids), lower-case Windows executable names and Android package
//! names. A browser, a Chromium web app, or an app the tables do not know is
//! categorised by its window title, so Gmail in Firefox counts as email and
//! WhatsApp Web as chat.
//!
//! Code editors and password managers are off until the user turns them on
//! ([`off_by_default`]).

use crate::io::AppInfo;
use crate::settings::AppCategory;

/// The category of the app that owns the focused field.
pub fn category(app: &AppInfo) -> AppCategory {
    let id = app.id.to_ascii_lowercase();
    match known(&id) {
        Some(category) => category,
        None if id.starts_with("jetbrains-") => AppCategory::Code,
        None => title_hint(&app.title).unwrap_or(AppCategory::Other),
    }
}

/// True for apps where Ghostcaret stays off unless the user turns it on:
/// code editors and password managers.
pub fn off_by_default(app: &AppInfo) -> bool {
    category(app) == AppCategory::Code || is_password_manager(&app.id.to_ascii_lowercase())
}

/// True when the app is a web browser, including Chromium web apps.
pub fn is_browser(app: &AppInfo) -> bool {
    let id = app.id.to_ascii_lowercase();
    browser(&id) || chromium_web_app(&id)
}

fn known(id: &str) -> Option<AppCategory> {
    use AppCategory::*;
    let category = match id {
        // Chat.
        "org.telegram.desktop"
        | "telegramdesktop"
        | "telegram-desktop"
        | "telegram.exe"
        | "org.telegram.messenger"
        | "org.telegram.messenger.web"
        | "org.thunderdog.challegram"
        | "discord"
        | "com.discordapp.discord"
        | "vesktop"
        | "dev.vencord.vesktop"
        | "webcord"
        | "discord.exe"
        | "com.discord"
        | "slack"
        | "com.slack.slack"
        | "com.slack"
        | "slack.exe"
        | "signal"
        | "signal-desktop"
        | "org.signal.signal"
        | "signal.exe"
        | "org.thoughtcrime.securesms"
        | "element"
        | "im.riot.riot"
        | "element.exe"
        | "im.vector.app"
        | "com.rtosta.zapzap"
        | "whatsapp-for-linux"
        | "com.github.eneshecan.whatsappforlinux"
        | "whatsapp.exe"
        | "whatsapp.root.exe"
        | "com.whatsapp"
        | "com.whatsapp.w4b"
        | "org.gnome.fractal"
        | "fractal"
        | "im.dino.dino"
        | "dino"
        | "org.kde.neochat"
        | "teams-for-linux"
        | "ms-teams.exe"
        | "teams.exe"
        | "com.microsoft.teams"
        | "skype.exe"
        | "com.skype.raider"
        | "com.google.android.apps.messaging"
        | "com.samsung.android.messaging"
        | "com.android.mms"
        | "com.facebook.orca"
        | "jp.naver.line.android"
        | "line.exe"
        | "com.viber.voip"
        | "viber.exe"
        | "com.tencent.mm"
        | "weixin.exe"
        | "wechat.exe"
        | "com.kakao.talk"
        | "kakaotalk.exe"
        | "zoom"
        | "zoom.exe"
        | "us.zoom.videomeetings"
        | "com.snapchat.android"
        | "chat.revolt.app"
        | "im.status.ethereum" => Chat,
        // Email.
        "thunderbird"
        | "org.mozilla.thunderbird"
        | "net.thunderbird.thunderbird"
        | "thunderbird.exe"
        | "net.thunderbird.android"
        | "com.fsck.k9"
        | "betterbird"
        | "eu.betterbird.betterbird"
        | "evolution"
        | "org.gnome.evolution"
        | "geary"
        | "org.gnome.geary"
        | "kmail"
        | "org.kde.kmail2"
        | "mailspring"
        | "com.getmailspring.mailspring"
        | "mailspring.exe"
        | "outlook.exe"
        | "olk.exe"
        | "hxoutlook.exe"
        | "com.microsoft.office.outlook"
        | "com.google.android.gm"
        | "ch.protonmail.android"
        | "me.proton.mail"
        | "com.samsung.android.email.provider"
        | "me.bluemail.mail"
        | "com.readdle.spark"
        | "em.client.exe"
        | "mailclient.exe" => Email,
        // Documents and notes.
        "obsidian"
        | "md.obsidian.obsidian"
        | "obsidian.exe"
        | "md.obsidian"
        | "logseq"
        | "com.logseq.logseq"
        | "logseq.exe"
        | "joplin"
        | "net.cozic.joplin_desktop"
        | "joplin.exe"
        | "net.cozic.joplin"
        | "notion-app"
        | "notion.exe"
        | "notion.id"
        | "libreoffice-writer"
        | "libreoffice"
        | "soffice"
        | "org.libreoffice.libreoffice"
        | "soffice.bin"
        | "swriter.exe"
        | "winword.exe"
        | "onenote.exe"
        | "com.microsoft.office.word"
        | "com.microsoft.office.onenote"
        | "com.google.android.apps.docs.editors.docs"
        | "com.google.android.keep"
        | "org.gnome.texteditor"
        | "gnome-text-editor"
        | "org.gnome.gedit"
        | "gedit"
        | "com.github.marktext.marktext"
        | "marktext"
        | "typora"
        | "io.typora.typora"
        | "apostrophe"
        | "org.gnome.gitlab.somas.apostrophe"
        | "org.standardnotes.standardnotes"
        | "zim"
        | "org.zim_wiki.zim"
        | "abiword"
        | "com.abisource.abiword"
        | "desktopeditors"
        | "org.onlyoffice.desktopeditors"
        | "wps"
        | "notepad.exe"
        | "wordpad.exe"
        | "evernote.exe"
        | "com.evernote"
        | "com.samsung.android.app.notes"
        | "anytype"
        | "io.anytype.anytype"
        | "affine"
        | "pro.affine.app"
        | "siyuan"
        | "org.b3log.siyuan"
        | "trilium"
        | "com.github.zadam.trilium"
        | "xournalpp"
        | "com.github.xournalpp.xournalpp"
        | "org.kde.ghostwriter" => Document,
        // Launchers and search.
        "rofi"
        | "wofi"
        | "fuzzel"
        | "tofi"
        | "anyrun"
        | "walker"
        | "ulauncher"
        | "albert"
        | "org.kde.krunner"
        | "krunner"
        | "launcher"
        | "com.google.android.googlequicksearchbox"
        | "searchhost.exe"
        | "powertoys.run.exe"
        | "everything.exe" => Search,
        // Terminals.
        "com.mitchellh.ghostty"
        | "ghostty"
        | "alacritty"
        | "org.alacritty.alacritty"
        | "alacritty.exe"
        | "kitty"
        | "foot"
        | "footclient"
        | "org.wezfurlong.wezterm"
        | "wezterm"
        | "wezterm-gui.exe"
        | "org.gnome.console"
        | "kgx"
        | "org.gnome.terminal"
        | "gnome-terminal-server"
        | "org.gnome.ptyxis"
        | "ptyxis"
        | "org.kde.konsole"
        | "konsole"
        | "org.kde.yakuake"
        | "xterm"
        | "uxterm"
        | "urxvt"
        | "st"
        | "st-256color"
        | "tilix"
        | "com.gexperts.tilix"
        | "terminator"
        | "rio"
        | "com.raggesilver.blackbox"
        | "dev.warp.warp"
        | "warp"
        | "warp.exe"
        | "contour"
        | "org.contourterminal.contour"
        | "guake"
        | "tabby"
        | "tabby.exe"
        | "xfce4-terminal"
        | "lxterminal"
        | "mate-terminal"
        | "windowsterminal.exe"
        | "wt.exe"
        | "cmd.exe"
        | "powershell.exe"
        | "pwsh.exe"
        | "conhost.exe"
        | "mintty.exe"
        | "com.termux" => Terminal,
        // Code editors.
        "code"
        | "code-oss"
        | "code-url-handler"
        | "visual-studio-code"
        | "com.visualstudio.code"
        | "com.visualstudio.code-oss"
        | "vscodium"
        | "codium"
        | "com.vscodium.codium"
        | "code.exe"
        | "cursor"
        | "cursor.exe"
        | "windsurf"
        | "windsurf.exe"
        | "zed"
        | "dev.zed.zed"
        | "zed.exe"
        | "sublime_text"
        | "com.sublimetext.three"
        | "sublime_text.exe"
        | "org.gnome.builder"
        | "neovide"
        | "emacs"
        | "gvim"
        | "org.kde.kate"
        | "kate"
        | "lapce"
        | "dev.lapce.lapce"
        | "devenv.exe"
        | "notepad++.exe"
        | "idea64.exe"
        | "pycharm64.exe"
        | "clion64.exe"
        | "goland64.exe"
        | "rider64.exe"
        | "webstorm64.exe"
        | "rustrover64.exe"
        | "studio64.exe"
        | "android-studio"
        | "com.google.androidstudio" => Code,
        _ => return None,
    };
    Some(category)
}

fn browser(id: &str) -> bool {
    matches!(
        id,
        "firefox"
            | "org.mozilla.firefox"
            | "firefox-esr"
            | "firefox-developer-edition"
            | "librewolf"
            | "io.gitlab.librewolf-community"
            | "zen"
            | "zen-browser"
            | "app.zen_browser.zen"
            | "floorp"
            | "waterfox"
            | "mullvad-browser"
            | "tor-browser"
            | "google-chrome"
            | "google-chrome-stable"
            | "google-chrome-beta"
            | "com.google.chrome"
            | "chromium"
            | "chromium-browser"
            | "org.chromium.chromium"
            | "helium"
            | "helium-browser"
            | "brave-browser"
            | "com.brave.browser"
            | "microsoft-edge"
            | "microsoft-edge-stable"
            | "com.microsoft.edge"
            | "vivaldi"
            | "vivaldi-stable"
            | "com.vivaldi.vivaldi"
            | "opera"
            | "thorium-browser"
            | "org.gnome.epiphany"
            | "epiphany"
            | "org.qutebrowser.qutebrowser"
            | "qutebrowser"
            | "org.kde.falkon"
            | "falkon"
            | "chrome.exe"
            | "msedge.exe"
            | "firefox.exe"
            | "brave.exe"
            | "opera.exe"
            | "vivaldi.exe"
            | "helium.exe"
            | "zen.exe"
            | "librewolf.exe"
            | "com.android.chrome"
            | "org.mozilla.firefox_beta"
            | "org.mozilla.fenix"
            | "com.microsoft.emmx"
            | "com.opera.browser"
            | "com.sec.android.app.sbrowser"
            | "com.vivaldi.browser"
            | "com.duckduckgo.mobile.android"
            | "app.vanadium.browser"
            | "org.chromium.chrome"
    )
}

/// Chromium installs each web app with its own id, such as
/// `chrome-mail.google.com__mail_u_0-default`.
fn chromium_web_app(id: &str) -> bool {
    ["chrome-", "chromium-", "msedge-", "brave-", "helium-"]
        .iter()
        .any(|p| id.starts_with(p))
        && id.ends_with("-default")
}

fn is_password_manager(id: &str) -> bool {
    matches!(
        id,
        "org.keepassxc.keepassxc"
            | "keepassxc"
            | "keepassxc.exe"
            | "keepass.exe"
            | "1password"
            | "1password.exe"
            | "com.onepassword.onepassword"
            | "com.agilebits.onepassword"
            | "bitwarden"
            | "com.bitwarden.desktop"
            | "bitwarden.exe"
            | "com.x8bit.bitwarden"
            | "enpass"
            | "enpass.exe"
            | "io.enpass.enpass"
            | "proton-pass"
            | "me.proton.pass"
            | "proton.android.pass"
            | "proton pass.exe"
            | "org.gnome.seahorse.application"
            | "seahorse"
            | "org.gnome.world.secrets"
            | "secrets"
            | "org.kde.kwalletmanager5"
            | "kwalletmanager5"
            | "org.kde.kwalletmanager"
            | "keepass2android.keepass2android"
            | "com.kunzisoft.keepass.free"
            | "com.kunzisoft.keepass.libre"
            | "com.lastpass.lpandroid"
            | "com.dashlane"
    )
}

/// Site names as browsers show them in window titles, such as
/// `Inbox (3) - me@example.com - Gmail - Mozilla Firefox`.
const SITES: &[(&str, AppCategory)] = &[
    ("gmail", AppCategory::Email),
    ("outlook", AppCategory::Email),
    ("proton mail", AppCategory::Email),
    ("fastmail", AppCategory::Email),
    ("yahoo mail", AppCategory::Email),
    ("google docs", AppCategory::Document),
    ("notion", AppCategory::Document),
    ("google keep", AppCategory::Document),
    ("confluence", AppCategory::Document),
    ("dropbox paper", AppCategory::Document),
    ("coda", AppCategory::Document),
    ("chatgpt", AppCategory::Chat),
    ("claude", AppCategory::Chat),
    ("gemini", AppCategory::Chat),
    ("whatsapp", AppCategory::Chat),
    ("whatsapp web", AppCategory::Chat),
    ("slack", AppCategory::Chat),
    ("discord", AppCategory::Chat),
    ("telegram", AppCategory::Chat),
    ("telegram web", AppCategory::Chat),
    ("linkedin", AppCategory::Chat),
    ("messenger", AppCategory::Chat),
    ("microsoft teams", AppCategory::Chat),
    ("element", AppCategory::Chat),
    ("google chat", AppCategory::Chat),
    ("google search", AppCategory::Search),
    ("duckduckgo", AppCategory::Search),
];

/// Category from a site name that is a whole part of the window title, with
/// parts separated by ` - `, ` | `, ` — `, ` – ` or ` · ` and unread counters
/// such as `(3)` ignored. Whole parts only, so a page about Claude Monet is
/// not a chat.
fn title_hint(title: &str) -> Option<AppCategory> {
    if title.is_empty() {
        return None;
    }
    let mut parts = vec![title];
    for sep in [" - ", " | ", " — ", " – ", " · "] {
        parts = parts.into_iter().flat_map(|p| p.split(sep)).collect();
    }
    parts.into_iter().find_map(|part| {
        let mut part = part.trim();
        if part.starts_with('(')
            && let Some(close) = part.find(')')
            && part[1..close]
                .chars()
                .all(|c| c.is_ascii_digit() || c == '+')
        {
            part = part[close + 1..].trim();
        }
        SITES
            .iter()
            .find(|(site, _)| site.eq_ignore_ascii_case(part))
            .map(|&(_, category)| category)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, title: &str) -> AppInfo {
        AppInfo {
            id: id.into(),
            name: String::new(),
            title: title.into(),
        }
    }

    #[test]
    fn known_apps_on_every_platform() {
        let table = [
            ("org.telegram.desktop", AppCategory::Chat),
            ("discord", AppCategory::Chat),
            ("Slack", AppCategory::Chat),
            ("signal", AppCategory::Chat),
            ("thunderbird", AppCategory::Email),
            ("obsidian", AppCategory::Document),
            ("com.mitchellh.ghostty", AppCategory::Terminal),
            ("Alacritty", AppCategory::Terminal),
            ("kitty", AppCategory::Terminal),
            ("foot", AppCategory::Terminal),
            ("code", AppCategory::Code),
            ("cursor", AppCategory::Code),
            ("jetbrains-idea", AppCategory::Code),
            ("slack.exe", AppCategory::Chat),
            ("outlook.exe", AppCategory::Email),
            ("winword.exe", AppCategory::Document),
            ("windowsterminal.exe", AppCategory::Terminal),
            ("code.exe", AppCategory::Code),
            ("com.google.android.apps.messaging", AppCategory::Chat),
            ("com.Slack", AppCategory::Chat),
            ("com.google.android.gm", AppCategory::Email),
            ("com.termux", AppCategory::Terminal),
            ("firefox", AppCategory::Other),
            ("something-unknown", AppCategory::Other),
        ];
        for (id, want) in table {
            assert_eq!(category(&app(id, "")), want, "{id}");
        }
    }

    #[test]
    fn browser_titles_name_the_site() {
        let table = [
            (
                "Inbox (3) - me@example.com - Gmail — Mozilla Firefox",
                AppCategory::Email,
            ),
            (
                "Mail - Anna Smith - Outlook - Google Chrome",
                AppCategory::Email,
            ),
            ("Project plan - Google Docs - Helium", AppCategory::Document),
            ("Trip ideas | Notion", AppCategory::Document),
            ("New chat - Claude", AppCategory::Chat),
            ("ChatGPT", AppCategory::Chat),
            ("(5) WhatsApp", AppCategory::Chat),
            ("general (Channel) - Acme - Slack", AppCategory::Chat),
            ("#general | Friends - Discord", AppCategory::Chat),
            ("Telegram Web", AppCategory::Chat),
            ("Messaging | LinkedIn", AppCategory::Chat),
            (
                "Claude Monet - Wikipedia - Mozilla Firefox",
                AppCategory::Other,
            ),
        ];
        for (title, want) in table {
            assert_eq!(category(&app("firefox", title)), want, "{title}");
        }
        assert_eq!(
            category(&app(
                "chrome-mail.google.com__mail_u_0-Default",
                "Inbox - Gmail"
            )),
            AppCategory::Email
        );
        assert!(is_browser(&app(
            "chrome-mail.google.com__mail_u_0-Default",
            ""
        )));
    }

    #[test]
    fn known_apps_ignore_titles() {
        assert_eq!(
            category(&app("org.telegram.desktop", "Gmail")),
            AppCategory::Chat
        );
    }

    #[test]
    fn code_editors_and_password_managers_are_off_by_default() {
        assert!(off_by_default(&app("code", "")));
        assert!(off_by_default(&app("org.keepassxc.keepassxc", "")));
        assert!(off_by_default(&app("com.x8bit.bitwarden", "")));
        assert!(!off_by_default(&app("firefox", "")));
        assert!(!off_by_default(&app("com.mitchellh.ghostty", "")));
    }
}
