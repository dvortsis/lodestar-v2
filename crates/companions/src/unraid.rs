//! A companion's filled Unraid template (plan 2026-11-06-v06-companions
//! §4.3 CMP-7 and Q11, architecture §18.10), from the same checked inputs
//! as its compose service ([`crate::compose`]).
//!
//! **No pre-filled link.** Unraid's Add Container page loads a template
//! only from a file already on the flash drive (`xmlTemplate=<type>:<path>`
//! is checked with `is_file`), and no other parameter fills a field; the
//! template-repository URL field went in Unraid 6.10. So Lodestar offers
//! the filled file to save into [`TEMPLATES_USER`] and the values to copy
//! ([`UnraidTemplate::fields`]); once the file is saved,
//! [`UnraidTemplate::add_container_path`] opens it. Sources are in
//! `docs/decisions/2026-11-08-CMP-7.md`.
//!
//! The XML is written only through `quick-xml`'s writer, which escapes
//! every text and attribute value. Unraid pastes `ExtraParams` into its
//! `docker create` line unescaped, so that field only ever holds fixed
//! flags, numbers and a [`ContainerName`](crate::inputs::ContainerName),
//! none of which can hold a space or a shell character.

use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesText, Event};

use crate::catalogue::{Catalogue, Proto, TemplateId};
use crate::compose::{
    Bind, Checked, GLUETUN, GLUETUN_CAP, GLUETUN_POST_RULES_TARGET, Route, ServiceInput,
    TemplateError, check_pair, printable, proto_name,
};
use crate::inputs::CompanionId;

/// Where gluetun's post-rules file is on your server, by default: the path
/// the user guide's download step saves it to. You can change it in the
/// form. A missing file fails closed: Docker makes a folder there, and
/// gluetun stops with "is a directory" before its VPN starts.
pub const POST_RULES_HOST: &str = "/mnt/cache/appdata/gluetun/post-rules.txt";

/// Where Unraid keeps your own templates, on the flash drive.
pub const TEMPLATES_USER: &str = "/boot/config/plugins/dockerMan/templates-user";

/// One template: the file to save, and what to copy if you fill the form
/// by hand instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnraidTemplate {
    /// `my-<name>.xml`, Unraid's own naming for a user template.
    pub file_name: String,
    pub xml: String,
    /// Field label and value, in the form's order. A VPN setting's value
    /// is empty: you fill it in.
    pub fields: Vec<(String, String)>,
}

impl UnraidTemplate {
    /// The path, on your Unraid server's web UI, that opens the saved
    /// template in Add Container.
    pub fn add_container_path(&self) -> String {
        format!(
            "/Docker/AddContainer?xmlTemplate=user:{TEMPLATES_USER}/{}",
            self.file_name
        )
    }
}

/// The templates for companion `id`: gluetun's first when `input.route`
/// is [`Route::Gluetun`], then the companion's.
///
/// A template is saved as `my-<name>.xml`, the name Community
/// Applications also uses, so saving it replaces a template of the same
/// name, and applying it replaces a container of the same name (with its
/// settings, a VPN key among them). To keep a gluetun you already run, use
/// [`Route::ExistingGluetun`].
pub fn unraid(
    catalogue: &Catalogue,
    id: &CompanionId,
    input: &ServiceInput,
) -> Result<Vec<UnraidTemplate>, TemplateError> {
    let (main, vpn) = check_pair(catalogue, id, input, TemplateId::UnraidTemplate)?;
    let net = input.network.as_str();
    let mut out = Vec::new();
    // Each flag is fixed text plus a closed ContainerName.
    let join = |name: &str| format!("--network=container:{name}");
    match (&vpn, &input.route) {
        // gluetun first: the companion's template can't be applied until
        // the container it shares a network with exists.
        (Some(v), _) => {
            let alias = format!("--network-alias={}", main.name.as_str());
            out.push(render(v, net, &[alias])?);
            out.push(render(&main, "none", &[join(v.name.as_str())])?);
        }
        (None, Route::ExistingGluetun(name)) => {
            out.push(render(&main, "none", &[join(name.as_str())])?);
        }
        (None, _) => out.push(render(&main, net, &[])?),
    }
    Ok(out)
}

type W = Writer<Vec<u8>>;

fn io<T>(r: std::io::Result<T>) -> Result<T, TemplateError> {
    r.map_err(|_| TemplateError::Encode)
}

fn element(w: &mut W, name: &str, text: &str) -> Result<(), TemplateError> {
    printable(text)?;
    let e = w.create_element(name);
    if text.is_empty() {
        io(e.write_empty())?;
    } else {
        io(e.write_text_content(BytesText::new(text)))?;
    }
    Ok(())
}

struct Config<'a> {
    name: &'a str,
    target: String,
    value: String,
    mode: &'a str,
    help: &'a str,
    kind: &'a str,
    required: bool,
    /// You fill it in: shown always, with no default.
    fill: bool,
    mask: bool,
}

fn config(w: &mut W, c: &Config<'_>) -> Result<(), TemplateError> {
    let display = if c.kind == "Variable" && !c.fill {
        "advanced"
    } else {
        "always"
    };
    let default = if c.fill { "" } else { c.value.as_str() };
    for v in [c.name, &c.target, &c.value, c.mode, c.help, c.kind] {
        printable(v)?;
    }
    let e = w.create_element("Config").with_attributes([
        ("Name", c.name),
        ("Target", c.target.as_str()),
        ("Default", default),
        ("Mode", c.mode),
        ("Description", c.help),
        ("Type", c.kind),
        ("Display", display),
        ("Required", if c.required { "true" } else { "false" }),
        ("Mask", if c.mask { "true" } else { "false" }),
    ]);
    if c.value.is_empty() {
        io(e.write_empty())?;
    } else {
        io(e.write_text_content(BytesText::new(&c.value)))?;
    }
    Ok(())
}

const PATH_NOTE: &str = "Mount real folders only, never a link to somewhere else.";
const REPLACE_NOTE: &str = "Applying it replaces a container or template you already have with the same name: check first.";
const NETWORK_NOTE: &str = "Create the Docker network first: Unraid starts a container on a network it doesn't know with none.";
const POST_RULES_NOTE: &str = "Before you start it, save Lodestar's post-rules file at the Post-rules path, as the Lodestar user guide shows (chapter 3, \"Close gluetun's gap when the VPN drops\"). Without the file, gluetun won't start.";
/// The start-order note for a container routed through gluetun container
/// `vpn` (a closed ContainerName, so the text stays fixed plus a name).
fn start_note(vpn: &str) -> String {
    format!(
        "Start {vpn} first. In Unraid's Docker autostart list, put {vpn} above this container and set {vpn}'s wait to at least 30 seconds, so its VPN firewall is on before this one starts."
    )
}
const VPN_NOTE: &str =
    "You fill this in, from your VPN provider. Lodestar never asks for or stores it.";

fn render(
    c: &Checked<'_>,
    network: &str,
    extra: &[String],
) -> Result<UnraidTemplate, TemplateError> {
    let mut args: Vec<String> = extra.to_vec();
    if c.profile.id == GLUETUN {
        args.push(format!("--cap-add={GLUETUN_CAP}"));
        args.push("--device=/dev/net/tun".to_owned());
    }
    let mut configs = Vec::new();
    for (m, path) in &c.mounts {
        configs.push(Config {
            name: m.label,
            target: m.target.to_owned(),
            value: path.as_str().to_owned(),
            mode: "rw",
            help: m.help,
            kind: "Path",
            required: true,
            fill: false,
            mask: false,
        });
    }
    if c.profile.id == GLUETUN {
        configs.push(Config {
            name: "Post-rules",
            target: GLUETUN_POST_RULES_TARGET.to_owned(),
            value: POST_RULES_HOST.to_owned(),
            mode: "ro",
            help: "Lodestar's post-rules file, read-only. Save it here before you start gluetun.",
            kind: "Path",
            required: true,
            fill: false,
            mask: false,
        });
    }
    for (key, value, vpn) in &c.env {
        configs.push(Config {
            name: key,
            target: (*key).to_owned(),
            value: if vpn.is_some() {
                String::new()
            } else {
                value.clone()
            },
            mode: "rw",
            help: if vpn.is_some() {
                VPN_NOTE
            } else {
                "Set by Lodestar."
            },
            kind: "Variable",
            required: true,
            fill: vpn.is_some(),
            mask: vpn.is_some_and(|(_, secret)| secret),
        });
    }
    let mut webui = String::new();
    for p in &c.publish {
        let mode = proto_name(p.proto);
        let (host, target) = (p.host, p.target);
        match p.bind {
            // Unraid's port fields publish on every address; a loopback
            // bind has no field, so it's a fixed flag of numbers.
            Bind::Loopback => {
                args.push(format!("--publish=127.0.0.1:{host}:{target}/{mode}"));
            }
            Bind::Lan => {
                if p.webui && p.proto == Proto::Tcp {
                    webui = format!("http://[IP]:[PORT:{target}]/");
                }
                configs.push(Config {
                    name: p.name,
                    target: target.to_string(),
                    value: host.to_string(),
                    mode,
                    help: "Published on your network.",
                    kind: "Port",
                    required: false,
                    fill: false,
                    mask: false,
                });
            }
        }
    }
    let homepage = c.entry.homepage.as_str();
    let mut overview = c.profile.overview.to_owned();
    if c.profile.id == GLUETUN {
        overview.push(' ');
        overview.push_str(POST_RULES_NOTE);
    }
    // Routed through a gluetun container: this one must start after
    // gluetun's firewall is on (Unraid has no health condition to wait on).
    if let Some(vpn) = extra
        .iter()
        .find_map(|a| a.strip_prefix("--network=container:"))
    {
        overview.push(' ');
        overview.push_str(&start_note(vpn));
    }
    if !c.mounts.is_empty() {
        overview.push(' ');
        overview.push_str(PATH_NOTE);
    }
    overview.push(' ');
    overview.push_str(REPLACE_NOTE);
    if network != "none" {
        overview.push(' ');
        overview.push_str(NETWORK_NOTE);
    }
    // Unraid pastes ExtraParams into its `docker create` line unescaped,
    // so each flag is held to a shell-inert allowlist, and the flags are
    // joined by single spaces.
    let shell_inert = |a: &String| {
        !a.is_empty()
            && a.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_=:./,".contains(&b))
    };
    if !args.iter().all(shell_inert) {
        return Err(TemplateError::ShellUnsafe);
    }
    let extra_params = args.join(" ");

    let mut w = Writer::new_with_indent(Vec::new(), b' ', 2);
    io(w.write_event(Event::Decl(BytesDecl::new("1.0", None, None))))?;
    let root = w
        .create_element("Container")
        .with_attribute(("version", "2"));
    io(root.write_inner_content(|w| {
        let mut body = || -> Result<(), TemplateError> {
            element(w, "Name", c.name.as_str())?;
            element(w, "Repository", &c.image)?;
            element(w, "Network", network)?;
            element(w, "MyIP", "")?;
            element(w, "Privileged", "false")?;
            element(w, "Support", homepage)?;
            element(w, "Project", homepage)?;
            element(w, "Overview", &overview)?;
            element(w, "Category", c.profile.category)?;
            element(w, "WebUI", &webui)?;
            element(w, "TemplateURL", "")?;
            element(w, "Icon", "")?;
            element(w, "ExtraParams", &extra_params)?;
            element(w, "PostArgs", "")?;
            for cfg in &configs {
                config(w, cfg)?;
            }
            Ok(())
        };
        body().map_err(|_| std::io::Error::other("xml"))
    }))?;
    let mut xml = String::from_utf8(w.into_inner()).map_err(|_| TemplateError::Encode)?;
    xml.push('\n');

    let mut fields = vec![
        ("Name".to_owned(), c.name.as_str().to_owned()),
        ("Repository".to_owned(), c.image.clone()),
        ("Network Type".to_owned(), network.to_owned()),
        ("Extra Parameters".to_owned(), extra_params),
    ];
    fields.extend(configs.iter().map(|k| (k.name.to_owned(), k.value.clone())));
    Ok(UnraidTemplate {
        file_name: format!("my-{}.xml", c.name.as_str()),
        xml,
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(c: &Config<'_>) -> String {
        let mut w = Writer::new(Vec::new());
        config(&mut w, c).expect("config");
        String::from_utf8(w.into_inner()).expect("utf8")
    }

    fn cfg(value: &str, fill: bool, mask: bool) -> Config<'_> {
        Config {
            name: "K",
            target: "K".to_owned(),
            value: value.to_owned(),
            mode: "rw",
            help: "a <b> & \"c\"",
            kind: "Variable",
            required: true,
            fill,
            mask,
        }
    }

    #[test]
    fn a_setting_you_fill_in_is_shown_empty_and_masked_if_secret() {
        let secret = written(&cfg("", true, true));
        assert!(secret.contains("Default=\"\""));
        assert!(secret.contains("Display=\"always\""));
        assert!(secret.contains("Mask=\"true\""));
        assert!(secret.ends_with("/>"));
        let provider = written(&cfg("", true, false));
        assert!(provider.contains("Mask=\"false\""));
        let fixed = written(&cfg("022", false, false));
        assert!(fixed.contains("Default=\"022\"") && fixed.contains("Display=\"advanced\""));
        assert!(fixed.ends_with(">022</Config>"));
    }

    #[test]
    fn control_characters_are_refused_not_written() {
        let mut w = Writer::new(Vec::new());
        assert_eq!(
            config(&mut w, &cfg("a\u{1}b", false, false)),
            Err(TemplateError::Unprintable)
        );
        assert_eq!(
            element(&mut w, "Name", "a\u{fffe}"),
            Err(TemplateError::Unprintable)
        );
    }

    #[test]
    fn attribute_values_are_escaped() {
        let x = written(&cfg("a<b", false, false));
        assert!(x.contains("Description=\"a &lt;b&gt; &amp; &quot;c&quot;\""));
        assert!(x.contains(">a&lt;b</Config>"));
    }

    #[test]
    fn the_link_names_the_saved_file() {
        let t = UnraidTemplate {
            file_name: "my-prowlarr.xml".to_owned(),
            xml: String::new(),
            fields: Vec::new(),
        };
        assert_eq!(
            t.add_container_path(),
            "/Docker/AddContainer?xmlTemplate=user:/boot/config/plugins/dockerMan/templates-user/my-prowlarr.xml"
        );
    }
}
