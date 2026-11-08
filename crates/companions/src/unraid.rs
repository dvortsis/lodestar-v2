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
    Bind, Checked, GLUETUN, GLUETUN_CAP, ServiceInput, TemplateError, check_pair, proto_name,
};
use crate::inputs::CompanionId;

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

/// The templates for companion `id`: its own, then gluetun's when
/// `input.route` is [`Route::Gluetun`](crate::compose::Route::Gluetun).
pub fn unraid(
    catalogue: &Catalogue,
    id: &CompanionId,
    input: &ServiceInput,
) -> Result<Vec<UnraidTemplate>, TemplateError> {
    let (main, vpn) = check_pair(catalogue, id, input, TemplateId::UnraidTemplate)?;
    let net = input.network.as_str();
    let mut out = Vec::new();
    match &vpn {
        Some(v) => {
            out.push(render(
                &main,
                "none",
                &["--network=container:gluetun".to_owned()],
            )?);
            let alias = format!("--network-alias={}", main.name.as_str());
            out.push(render(v, net, &[alias])?);
        }
        None => out.push(render(&main, net, &[])?),
    }
    Ok(out)
}

type W = Writer<Vec<u8>>;

fn io<T>(r: std::io::Result<T>) -> Result<T, TemplateError> {
    r.map_err(|_| TemplateError::Encode)
}

fn element(w: &mut W, name: &str, text: &str) -> Result<(), TemplateError> {
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
    mask: bool,
}

fn config(w: &mut W, c: &Config<'_>) -> Result<(), TemplateError> {
    let display = if c.kind == "Variable" && !c.mask {
        "advanced"
    } else {
        "always"
    };
    let default = if c.mask { "" } else { c.value.as_str() };
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
const NETWORK_NOTE: &str = "Create the Docker network first: Unraid starts a container on a network it doesn't know with none.";
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
            mask: false,
        });
    }
    for (key, value, help) in &c.env {
        let vpn = help.is_some();
        configs.push(Config {
            name: key,
            target: (*key).to_owned(),
            value: if vpn { String::new() } else { value.clone() },
            mode: "",
            help: if vpn { VPN_NOTE } else { "Set by Lodestar." },
            kind: "Variable",
            required: true,
            mask: vpn,
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
                    mask: false,
                });
            }
        }
    }
    let homepage = c.entry.homepage.as_str();
    let mut overview = c.profile.overview.to_owned();
    if !c.mounts.is_empty() {
        overview.push(' ');
        overview.push_str(PATH_NOTE);
    }
    if network != "none" {
        overview.push(' ');
        overview.push_str(NETWORK_NOTE);
    }
    let extra_params = args.join(" ");
    if args.iter().any(|a| a.contains(char::is_whitespace)) {
        return Err(TemplateError::Encode);
    }

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
