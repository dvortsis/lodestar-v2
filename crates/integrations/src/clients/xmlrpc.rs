//! The XML-RPC subset rTorrent speaks: requests written by hand, answers
//! read with `quick-xml` (§16).
//!
//! Answers are untrusted. The caller bounds their size with `read_limited`;
//! the parser bounds nesting at [`MAX_DEPTH`] arrays or structs, refuses any
//! `<!DOCTYPE` (so no DTD and no entity definitions), resolves only the five
//! predefined entities and character references, and accepts only the value
//! types rTorrent sends: `i4`/`int`/`i8`, `string` (or bare text),
//! `boolean`, `double`, `nil`, `array` and `struct`. Anything else is an
//! error, never a panic.

use quick_xml::Reader;
use quick_xml::events::Event;

/// How deeply arrays and structs may nest in an answer. rTorrent's deepest
/// is a `system.multicall` result: an array of arrays of values.
pub(crate) const MAX_DEPTH: usize = 16;

/// A value in an answer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Int(i64),
    Bool(bool),
    Str(String),
    Double(f64),
    Nil,
    Array(Vec<Value>),
    Struct(Vec<(String, Value)>),
}

impl Value {
    pub(crate) fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    fn member(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Struct(members) => members.iter().find(|(n, _)| n == name).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The fault a `system.multicall` entry carries instead of a result.
    pub(crate) fn as_fault(&self) -> Option<Fault> {
        Some(Fault {
            code: self.member("faultCode")?.as_int()?,
            message: self.member("faultString")?.as_str()?.to_owned(),
        })
    }
}

/// A `<fault>` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fault {
    pub(crate) code: i64,
    pub(crate) message: String,
}

/// A parsed `methodResponse`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Response {
    Value(Value),
    Fault(Fault),
}

/// A request parameter.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Param<'a> {
    Str(&'a str),
    Int(i64),
    Base64(&'a [u8]),
}

/// A `system.multicall` entry: a method and its parameters.
pub(crate) type Call<'a> = (&'a str, Vec<Param<'a>>);

/// A `methodCall` document. Text with characters XML 1.0 can't carry is
/// refused (the adapter checks its inputs first; this is the backstop).
pub(crate) fn method_call(method: &str, params: &[Param<'_>]) -> Result<String, &'static str> {
    let mut out = String::from("<?xml version=\"1.0\"?><methodCall><methodName>");
    text(&mut out, method)?;
    out.push_str("</methodName><params>");
    for p in params {
        out.push_str("<param>");
        param(&mut out, p)?;
        out.push_str("</param>");
    }
    out.push_str("</params></methodCall>");
    Ok(out)
}

/// A `system.multicall` document.
pub(crate) fn multicall(calls: &[Call<'_>]) -> Result<String, &'static str> {
    let mut out = String::from(
        "<?xml version=\"1.0\"?><methodCall><methodName>system.multicall</methodName><params><param><value><array><data>",
    );
    for (method, params) in calls {
        out.push_str("<value><struct><member><name>methodName</name><value><string>");
        text(&mut out, method)?;
        out.push_str("</string></value></member><member><name>params</name><value><array><data>");
        for p in params {
            param(&mut out, p)?;
        }
        out.push_str("</data></array></value></member></struct></value>");
    }
    out.push_str("</data></array></value></param></params></methodCall>");
    Ok(out)
}

fn param(out: &mut String, p: &Param<'_>) -> Result<(), &'static str> {
    out.push_str("<value>");
    match p {
        Param::Str(s) => {
            out.push_str("<string>");
            text(out, s)?;
            out.push_str("</string>");
        }
        Param::Int(n) => {
            out.push_str("<i8>");
            out.push_str(&n.to_string());
            out.push_str("</i8>");
        }
        Param::Base64(bytes) => {
            out.push_str("<base64>");
            out.push_str(&super::base64(bytes));
            out.push_str("</base64>");
        }
    }
    out.push_str("</value>");
    Ok(())
}

fn text(out: &mut String, s: &str) -> Result<(), &'static str> {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if c.is_control() => return Err("text with control characters"),
            c => out.push(c),
        }
    }
    Ok(())
}

/// Parses a `methodResponse`. Errors are short fixed reasons.
pub(crate) fn parse_response(body: &[u8]) -> Result<Response, &'static str> {
    let text = std::str::from_utf8(body).map_err(|_| "the answer isn't UTF-8")?;
    let mut p = Parser {
        reader: Reader::from_str(text),
        peeked: None,
    };
    p.expect_start("methodResponse")?;
    let response = match p.next_tag()? {
        Tag::Start(name) if name == "params" => {
            let value = match p.next_tag()? {
                Tag::Start(name) if name == "param" => {
                    let v = p.value(0)?;
                    p.expect_end("param")?;
                    p.expect_end("params")?;
                    v
                }
                Tag::End(name) if name == "params" => Value::Nil,
                _ => return Err("expected a param"),
            };
            Response::Value(value)
        }
        Tag::Empty(name) if name == "params" => Response::Value(Value::Nil),
        Tag::Start(name) if name == "fault" => {
            let v = p.value(0)?;
            p.expect_end("fault")?;
            Response::Fault(
                v.as_fault()
                    .ok_or("the fault isn't a faultCode and faultString")?,
            )
        }
        _ => return Err("expected params or a fault"),
    };
    p.expect_end("methodResponse")?;
    match p.next_tag()? {
        Tag::Eof => Ok(response),
        _ => Err("content after the response"),
    }
}

/// A structural token: element boundaries, or text merged from adjacent
/// text, CDATA and reference events.
#[derive(Debug)]
enum Token {
    Start(String),
    End(String),
    Empty(String),
    Text(String),
    Eof,
}

/// A token in a structural position: whitespace between elements skipped.
#[derive(Debug)]
enum Tag {
    Start(String),
    End(String),
    Empty(String),
    Eof,
}

struct Parser<'a> {
    reader: Reader<&'a [u8]>,
    peeked: Option<Token>,
}

impl Parser<'_> {
    fn next(&mut self) -> Result<Token, &'static str> {
        if let Some(t) = self.peeked.take() {
            return Ok(t);
        }
        let mut text: Option<String> = None;
        loop {
            let event = self.reader.read_event().map_err(|_| "malformed XML")?;
            let piece = match event {
                Event::Text(t) => t.xml10_content().into_owned(),
                Event::CData(t) => t.into_inner().into_owned(),
                Event::GeneralRef(r) => match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    Ok(None) => predefined(&r)?.to_owned(),
                    Err(_) => return Err("a bad character reference"),
                },
                Event::DocType(_) => return Err("a DOCTYPE"),
                Event::Decl(_) | Event::Comment(_) | Event::PI(_) => continue,
                other => {
                    let token = match other {
                        Event::Start(e) => Token::Start(e.name().as_ref().to_owned()),
                        Event::End(e) => Token::End(e.name().as_ref().to_owned()),
                        Event::Empty(e) => Token::Empty(e.name().as_ref().to_owned()),
                        _ => Token::Eof,
                    };
                    return Ok(match text {
                        Some(t) => {
                            self.peeked = Some(token);
                            Token::Text(t)
                        }
                        None => token,
                    });
                }
            };
            text.get_or_insert_with(String::new).push_str(&piece);
        }
    }

    fn next_tag(&mut self) -> Result<Tag, &'static str> {
        loop {
            return Ok(match self.next()? {
                Token::Text(t) if t.trim().is_empty() => continue,
                Token::Text(_) => return Err("text where an element belongs"),
                Token::Start(n) => Tag::Start(n),
                Token::End(n) => Tag::End(n),
                Token::Empty(n) => Tag::Empty(n),
                Token::Eof => Tag::Eof,
            });
        }
    }

    fn expect_start(&mut self, name: &str) -> Result<(), &'static str> {
        match self.next_tag()? {
            Tag::Start(n) if n == name => Ok(()),
            _ => Err("an unexpected element"),
        }
    }

    fn expect_end(&mut self, name: &str) -> Result<(), &'static str> {
        match self.next_tag()? {
            Tag::End(n) if n == name => Ok(()),
            _ => Err("an unexpected element"),
        }
    }

    /// Text up to the end of `name` (which may be empty).
    fn text_until(&mut self, name: &str) -> Result<String, &'static str> {
        match self.next()? {
            Token::Text(t) => {
                self.expect_end(name)?;
                Ok(t)
            }
            Token::End(n) if n == name => Ok(String::new()),
            _ => Err("an element inside a scalar"),
        }
    }

    /// A `<value>` element, whose start tag is next.
    fn value(&mut self, depth: usize) -> Result<Value, &'static str> {
        let tag = self.next_tag()?;
        self.value_from(tag, depth)
    }

    /// A `<value>` element whose start tag `tag` was just read.
    fn value_from(&mut self, tag: Tag, depth: usize) -> Result<Value, &'static str> {
        match tag {
            Tag::Start(n) if n == "value" => {}
            Tag::Empty(n) if n == "value" => return Ok(Value::Str(String::new())),
            _ => return Err("expected a value"),
        }
        let v = match self.next()? {
            // Bare text is a string.
            Token::Text(t) => match self.next()? {
                Token::End(n) if n == "value" => return Ok(Value::Str(t)),
                Token::Start(n) if t.trim().is_empty() => self.typed(&n, depth)?,
                Token::Empty(n) if t.trim().is_empty() => empty(&n)?,
                _ => return Err("text and an element in one value"),
            },
            Token::End(n) if n == "value" => return Ok(Value::Str(String::new())),
            Token::Start(n) => self.typed(&n, depth)?,
            Token::Empty(n) => empty(&n)?,
            _ => return Err("expected a value"),
        };
        self.expect_end("value")?;
        Ok(v)
    }

    /// The typed content of a value, whose start tag `name` was just read.
    fn typed(&mut self, name: &str, depth: usize) -> Result<Value, &'static str> {
        Ok(match name {
            "i4" | "int" | "i8" | "ex:i8" => Value::Int(
                self.text_until(name)?
                    .trim()
                    .parse()
                    .map_err(|_| "an integer that isn't one")?,
            ),
            "boolean" => match self.text_until(name)?.trim() {
                "0" => Value::Bool(false),
                "1" => Value::Bool(true),
                _ => return Err("a boolean that isn't 0 or 1"),
            },
            "double" => Value::Double(
                self.text_until(name)?
                    .trim()
                    .parse()
                    .map_err(|_| "a double that isn't one")?,
            ),
            "string" => Value::Str(self.text_until(name)?),
            "nil" | "ex:nil" => {
                self.expect_end(name)?;
                Value::Nil
            }
            "array" => {
                let depth = nest(depth)?;
                let mut items = Vec::new();
                match self.next_tag()? {
                    Tag::Start(n) if n == "data" => loop {
                        match self.next_tag()? {
                            Tag::End(n) if n == "data" => break,
                            tag => items.push(self.value_from(tag, depth)?),
                        }
                    },
                    Tag::Empty(n) if n == "data" => {}
                    _ => return Err("an array without data"),
                }
                self.expect_end("array")?;
                Value::Array(items)
            }
            "struct" => {
                let depth = nest(depth)?;
                let mut members = Vec::new();
                loop {
                    match self.next_tag()? {
                        Tag::End(n) if n == "struct" => break,
                        Tag::Start(n) if n == "member" => {
                            self.expect_start("name")?;
                            let key = self.text_until("name")?;
                            let v = self.value(depth)?;
                            self.expect_end("member")?;
                            members.push((key, v));
                        }
                        _ => return Err("a struct entry that isn't a member"),
                    }
                }
                Value::Struct(members)
            }
            _ => return Err("an unsupported value type"),
        })
    }
}

/// A self-closed typed element: an empty string or array, or nil.
fn empty(name: &str) -> Result<Value, &'static str> {
    match name {
        "string" => Ok(Value::Str(String::new())),
        "nil" | "ex:nil" => Ok(Value::Nil),
        "array" => Ok(Value::Array(Vec::new())),
        "struct" => Ok(Value::Struct(Vec::new())),
        _ => Err("an empty scalar"),
    }
}

fn nest(depth: usize) -> Result<usize, &'static str> {
    if depth >= MAX_DEPTH {
        Err("values nested too deeply")
    } else {
        Ok(depth + 1)
    }
}

/// The five entities XML predefines. No others exist without a DTD, and a
/// DTD is refused.
fn predefined(name: &str) -> Result<&'static str, &'static str> {
    match name {
        "lt" => Ok("<"),
        "gt" => Ok(">"),
        "amp" => Ok("&"),
        "quot" => Ok("\""),
        "apos" => Ok("'"),
        _ => Err("an undefined entity"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ok(params: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<methodResponse>\r\n<params>\r\n<param>{params}</param>\r\n</params>\r\n</methodResponse>\r\n"
        )
    }

    fn value(v: &str) -> Result<Value, &'static str> {
        match parse_response(ok(v).as_bytes())? {
            Response::Value(v) => Ok(v),
            Response::Fault(_) => panic!("a fault"),
        }
    }

    #[test]
    fn scalars() {
        assert_eq!(
            value("<value><string>0.9.8</string></value>"),
            Ok(Value::Str("0.9.8".into()))
        );
        assert_eq!(value("<value>bare</value>"), Ok(Value::Str("bare".into())));
        assert_eq!(value("<value></value>"), Ok(Value::Str(String::new())));
        assert_eq!(value("<value/>"), Ok(Value::Str(String::new())));
        assert_eq!(
            value("<value><string/></value>"),
            Ok(Value::Str(String::new()))
        );
        assert_eq!(
            value("<value><string>  </string></value>"),
            Ok(Value::Str("  ".into()))
        );
        assert_eq!(value("<value><i4>-5</i4></value>"), Ok(Value::Int(-5)));
        assert_eq!(
            value("<value> <i8>8589934592</i8> </value>"),
            Ok(Value::Int(1 << 33))
        );
        assert_eq!(value("<value><int>7</int></value>"), Ok(Value::Int(7)));
        assert_eq!(
            value("<value><boolean>1</boolean></value>"),
            Ok(Value::Bool(true))
        );
        assert_eq!(
            value("<value><double>1.5</double></value>"),
            Ok(Value::Double(1.5))
        );
        assert_eq!(value("<value><nil/></value>"), Ok(Value::Nil));
        assert_eq!(
            value(
                "<value><string>a &amp; b &lt;c&gt; &#233;&#x41;<![CDATA[<x>]]></string></value>"
            ),
            Ok(Value::Str("a & b <c> éA<x>".into()))
        );
        for bad in [
            "<value><i4>x</i4></value>",
            "<value><i4>99999999999999999999</i4></value>",
            "<value><boolean>2</boolean></value>",
            "<value><base64>AA==</base64></value>",
            "<value><dateTime.iso8601>x</dateTime.iso8601></value>",
            "<value><string>a<b/></string></value>",
            "<value>text<string>x</string></value>",
            "<value><string>&foo;</string></value>",
            "<value><string>&#0;</string></value>",
            "<value><i4/></value>",
        ] {
            assert!(value(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn arrays_structs_and_multicall_results() {
        let v = value(
            "<value><array><data>\r\n<value><array><data>\r\n<value><i4>0</i4></value>\r\n</data></array></value>\r\n<value><array><data/></array></value></data></array></value>",
        )
        .unwrap();
        assert_eq!(
            v,
            Value::Array(vec![
                Value::Array(vec![Value::Int(0)]),
                Value::Array(vec![])
            ])
        );
        let v = value("<value><struct><member><name>faultCode</name><value><i4>-501</i4></value></member><member><name>faultString</name><value><string>Could not find info-hash.</string></value></member></struct></value>").unwrap();
        assert_eq!(
            v.as_fault(),
            Some(Fault {
                code: -501,
                message: "Could not find info-hash.".into()
            })
        );
        assert_eq!(
            value("<value><array><data></data></array></value>"),
            Ok(Value::Array(vec![]))
        );
        assert!(value("<value><array></array></value>").is_err());
        assert!(value("<value><struct><value><i4>1</i4></value></struct></value>").is_err());
    }

    #[test]
    fn faults_and_documents() {
        let fault = "<?xml version=\"1.0\"?><methodResponse><fault><value><struct><member><name>faultCode</name><value><i4>-506</i4></value></member><member><name>faultString</name><value><string>Method 'x' not defined</string></value></member></struct></value></fault></methodResponse>";
        assert_eq!(
            parse_response(fault.as_bytes()),
            Ok(Response::Fault(Fault {
                code: -506,
                message: "Method 'x' not defined".into()
            }))
        );
        assert_eq!(
            parse_response(b"<methodResponse><params/></methodResponse>"),
            Ok(Response::Value(Value::Nil))
        );
        assert!(
            parse_response(
                b"<methodResponse><fault><value><i4>1</i4></value></fault></methodResponse>"
            )
            .is_err()
        );
        for bad in [
            &b""[..],
            b"<html><body>502 Bad Gateway</body></html>",
            b"<methodResponse><params><param><value><i4>1</i4></value></param></params>",
            b"<methodResponse><params><param><value><i4>1</i4></value></param></params></methodResponse><x/>",
            b"<methodResponse><params><param><value><i4>1</i4></param></value></params></methodResponse>",
            b"\xff\xfe<methodResponse/>",
            b"<?xml version=\"1.0\"?><!DOCTYPE r [<!ENTITY a \"aaaa\">]><methodResponse><params><param><value><string>&a;</string></value></param></params></methodResponse>",
            b"<!doctype methodResponse SYSTEM \"file:///etc/passwd\"><methodResponse/>",
        ] {
            assert!(parse_response(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        // Comments and processing instructions are skipped.
        assert_eq!(
            parse_response(&ok("<!-- c --><value><?pi x?><i4>1</i4></value>").into_bytes()),
            Ok(Response::Value(Value::Int(1)))
        );
    }

    fn nested(levels: usize) -> String {
        let open = "<value><array><data>".repeat(levels);
        let close = "</data></array></value>".repeat(levels);
        ok(&format!("{open}<value><i4>1</i4></value>{close}"))
    }

    #[test]
    fn nesting_is_limited() {
        assert!(parse_response(nested(MAX_DEPTH).as_bytes()).is_ok());
        assert_eq!(
            parse_response(nested(MAX_DEPTH + 1).as_bytes()),
            Err("values nested too deeply")
        );
        // Far past the limit: refused at the limit, without recursing on.
        assert!(parse_response(nested(100_000).as_bytes()).is_err());
    }

    #[test]
    fn requests_are_escaped_and_typed() {
        let doc = method_call(
            "load.start",
            &[
                Param::Str(""),
                Param::Str("magnet:?xt=urn:btih:ab&dn=<x>"),
                Param::Int(3),
                Param::Base64(b"foo"),
            ],
        )
        .unwrap();
        assert_eq!(
            doc,
            "<?xml version=\"1.0\"?><methodCall><methodName>load.start</methodName><params><param><value><string></string></value></param><param><value><string>magnet:?xt=urn:btih:ab&amp;dn=&lt;x&gt;</string></value></param><param><value><i8>3</i8></value></param><param><value><base64>Zm9v</base64></value></param></params></methodCall>"
        );
        assert!(method_call("x", &[Param::Str("a\u{0}b")]).is_err());
        let doc =
            multicall(&[("f.priority.set", vec![Param::Str("AB:f1"), Param::Int(0)])]).unwrap();
        assert!(doc.contains("<member><name>methodName</name><value><string>f.priority.set</string></value></member><member><name>params</name><value><array><data><value><string>AB:f1</string></value><value><i8>0</i8></value></data></array></value></member>"));
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = parse_response(&bytes);
        }

        #[test]
        fn arbitrary_markup_never_panics(parts in proptest::collection::vec(
            prop_oneof![
                Just("<methodResponse>"), Just("</methodResponse>"), Just("<params>"),
                Just("</params>"), Just("<param>"), Just("</param>"), Just("<value>"),
                Just("</value>"), Just("<array>"), Just("</array>"), Just("<data>"),
                Just("</data>"), Just("<struct>"), Just("</struct>"), Just("<member>"),
                Just("</member>"), Just("<name>"), Just("</name>"), Just("<i4>"), Just("</i4>"),
                Just("<string>"), Just("</string>"), Just("1"), Just("&amp;"), Just("&#x0;"),
                Just("<fault>"), Just("</fault>"), Just("<data/>"), Just("<value/>"), Just(" "),
            ],
            0..64,
        )) {
            let _ = parse_response(parts.concat().as_bytes());
        }

        #[test]
        fn nesting_past_the_limit_is_refused(extra in 1usize..64) {
            prop_assert_eq!(
                parse_response(nested(MAX_DEPTH + extra).as_bytes()),
                Err("values nested too deeply")
            );
        }

        #[test]
        fn strings_round_trip(s in "[^\\p{Cc}]{0,64}") {
            let doc = method_call("m", &[Param::Str(&s)]).unwrap();
            // A request's string, wrapped as an answer, reads back the same.
            let start = doc.find("<value>").unwrap();
            let end = doc.rfind("</value>").unwrap() + "</value>".len();
            prop_assert_eq!(value(&doc[start..end]), Ok(Value::Str(s)));
        }
    }
}
