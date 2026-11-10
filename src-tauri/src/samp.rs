use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, Url};
use roxmltree::{Document, Node};
use serde_json::{Map, Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct Samp {
    lock: Option<PathBuf>,
    probe: Arc<Mutex<Probe>>,
}

#[derive(Default)]
struct Probe {
    checked: Option<Instant>,
    checking: bool,
    connected: bool,
    error: Option<String>,
}

impl Samp {
    pub fn at(lockfile: PathBuf) -> Self {
        Self {
            lock: Some(lockfile),
            ..Self::default()
        }
    }

    fn path(&self) -> Result<PathBuf> {
        self.lock.clone().map(Ok).unwrap_or_else(lockfile)
    }

    pub fn connected(&self) -> bool {
        let mut probe = self.probe.lock().unwrap();
        if !probe.checking
            && probe
                .checked
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(2))
        {
            probe.checking = true;
            let samp = self.clone();
            tokio::spawn(async move {
                let result = samp.ping().await;
                let mut probe = samp.probe.lock().unwrap();
                let (connected, error) = match result {
                    Ok(connected) => (connected, None),
                    Err(error) => (false, Some(format!("{error:#}"))),
                };
                if error != probe.error
                    && let Some(message) = &error
                {
                    eprintln!("NightBloom Aladin status: {message}");
                }
                probe.connected = connected;
                probe.error = error;
                probe.checked = Some(Instant::now());
                probe.checking = false;
            });
        }
        probe.connected
    }
}

fn escaped(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) fn encode(value: &Value) -> Result<String> {
    let body = match value {
        Value::String(value) => format!("<string>{}</string>", escaped(value)),
        Value::Object(values) => {
            let mut body = String::from("<struct>");
            for (key, value) in values {
                body.push_str(&format!(
                    "<member><name>{}</name>{}</member>",
                    escaped(key),
                    encode(value)?
                ));
            }
            body.push_str("</struct>");
            body
        }
        Value::Array(values) => {
            let mut body = String::from("<array><data>");
            for value in values {
                body.push_str(&encode(value)?);
            }
            body.push_str("</data></array>");
            body
        }
        Value::Bool(value) => format!("<boolean>{}</boolean>", u8::from(*value)),
        Value::Number(value) => {
            if value.is_i64() || value.is_u64() {
                format!("<int>{value}</int>")
            } else {
                format!("<double>{value}</double>")
            }
        }
        Value::Null => bail!("SAMP does not support null parameters."),
    };
    Ok(format!("<value>{body}</value>"))
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Result<Node<'a, 'input>> {
    node.children()
        .find(|child| child.has_tag_name(name))
        .context("Invalid local SAMP response")
}

pub(crate) fn decode(node: Node<'_, '_>, depth: usize) -> Result<Value> {
    ensure!(depth < 32, "Local SAMP response is nested too deeply.");
    let Some(kind) = node.children().find(Node::is_element) else {
        return Ok(json!(node.text().unwrap_or("")));
    };
    match kind.tag_name().name() {
        "string" | "base64" | "dateTime.iso8601" => Ok(json!(kind.text().unwrap_or(""))),
        "int" | "i4" | "i8" => Ok(json!(
            kind.text()
                .unwrap_or("")
                .trim()
                .parse::<i64>()
                .context("Invalid SAMP integer")?
        )),
        "double" => {
            let number: f64 = kind
                .text()
                .unwrap_or("")
                .trim()
                .parse()
                .context("Invalid SAMP number")?;
            ensure!(number.is_finite(), "Invalid SAMP number");
            Ok(json!(number))
        }
        "boolean" => match kind.text().unwrap_or("").trim() {
            "0" => Ok(json!(false)),
            "1" => Ok(json!(true)),
            _ => bail!("Invalid SAMP boolean"),
        },
        "struct" => {
            let mut values = Map::new();
            for member in kind.children().filter(|n| n.has_tag_name("member")) {
                let name = child(member, "name")?.text().unwrap_or("").to_owned();
                let value = decode(child(member, "value")?, depth + 1)?;
                ensure!(
                    values.insert(name, value).is_none(),
                    "Duplicate SAMP member"
                );
            }
            Ok(Value::Object(values))
        }
        "array" => Ok(Value::Array(
            child(kind, "data")?
                .children()
                .filter(|n| n.has_tag_name("value"))
                .map(|n| decode(n, depth + 1))
                .collect::<Result<_>>()?,
        )),
        _ => bail!("Unsupported local SAMP value type"),
    }
}

fn response(xml: &str) -> Result<Value> {
    let document = Document::parse(xml).context("Invalid XML from the local SAMP hub")?;
    let root = document.root_element();
    ensure!(
        root.has_tag_name("methodResponse"),
        "Unexpected local SAMP response"
    );
    if root.children().any(|node| node.has_tag_name("fault")) {
        // A hub fault may echo its private-key argument. Never put it in logs or HTTP errors.
        bail!("The local SAMP hub rejected the request. Reconnect Aladin with SAMP.");
    }
    decode(child(child(child(root, "params")?, "param")?, "value")?, 0)
}

fn local_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("Invalid SAMP hub address")?;
    let local = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && local
            && url.username().is_empty()
            && url.password().is_none(),
        "The SAMP hub must use a loopback address."
    );
    Ok(url)
}

fn lockfile() -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("SAMP_HUB") {
        let value = value.to_str().context("Invalid SAMP_HUB setting")?;
        let url = value
            .strip_prefix("std-lockurl:")
            .context("Enable Aladin's standard SAMP hub.")?;
        return Url::parse(url)?
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("SAMP_HUB must point to a local SAMP lock file."));
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .context("Cannot locate the SAMP lock file.")?;
    Ok(PathBuf::from(home).join(".samp"))
}

struct SampClient {
    client: Client,
    endpoint: Url,
    private_key: String,
}

impl SampClient {
    async fn connect(path: PathBuf) -> Result<Self> {
        let text = tokio::fs::read_to_string(path)
            .await
            .context("Cannot find a SAMP hub. Enable Connect with SAMP in Aladin.")?;
        let properties: std::collections::HashMap<_, _> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.trim(), value.trim()))
            .collect();
        let secret = properties
            .get("samp.secret")
            .context("The local SAMP lock file has no secret.")?;
        let endpoint = local_url(
            properties
                .get("samp.hub.xmlrpc.url")
                .context("The local SAMP lock file has no hub address.")?,
        )?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(15))
            .build()?;
        let mut connection = Self {
            client,
            endpoint,
            private_key: String::new(),
        };
        let registration = connection.rpc("register", &[json!(secret)]).await?;
        connection.private_key = registration["samp.private-key"]
            .as_str()
            .filter(|key| !key.is_empty())
            .context("The local SAMP hub did not register NightBloom.")?
            .to_owned();
        let result = connection
            .call(
                "declareMetadata",
                &[json!({"samp.name":"NightBloom","samp.description.text":"Astronomy art editor"})],
            )
            .await;
        if let Err(error) = result {
            return connection.finish(Err(error)).await;
        }
        Ok(connection)
    }

    async fn rpc(&self, method: &str, params: &[Value]) -> Result<Value> {
        let mut body = format!(
            "<?xml version=\"1.0\"?><methodCall><methodName>samp.hub.{method}</methodName><params>"
        );
        for param in params {
            body.push_str("<param>");
            body.push_str(&encode(param)?);
            body.push_str("</param>");
        }
        body.push_str("</params></methodCall>");
        let mut reply = self
            .client
            .post(self.endpoint.clone())
            .header(reqwest::header::CONTENT_TYPE, "text/xml")
            .body(body)
            .send()
            .await
            .map_err(|error| error.without_url())
            .context("The local SAMP hub is not responding. Reconnect Aladin with SAMP.")?
            .error_for_status()
            .map_err(|error| error.without_url())?;
        let mut bytes = Vec::new();
        while let Some(chunk) = reply.chunk().await? {
            ensure!(
                chunk.len() <= (128 * 1024 * 1024_usize).saturating_sub(bytes.len()),
                "Aladin's SAMP response exceeds 128 MiB."
            );
            bytes.extend_from_slice(&chunk);
        }
        response(std::str::from_utf8(&bytes).context("SAMP returned invalid UTF-8")?)
    }

    async fn call(&self, method: &str, params: &[Value]) -> Result<Value> {
        let mut all = vec![json!(self.private_key)];
        all.extend_from_slice(params);
        self.rpc(method, &all).await
    }

    async fn aladin(&self, mtype: &str, check: bool) -> Result<Vec<String>> {
        let subscribed = self.call("getSubscribedClients", &[json!(mtype)]).await?;
        let candidates = subscribed
            .as_object()
            .context("The local SAMP hub returned invalid subscriptions.")?;
        let mut found = Vec::new();
        for identifier in candidates.keys() {
            let metadata = self.call("getMetadata", &[json!(identifier)]).await?;
            if metadata["samp.name"]
                .as_str()
                .unwrap_or("")
                .to_ascii_lowercase()
                .contains("aladin")
            {
                found.push(identifier.clone());
            }
        }
        if found.len() <= 1 && !check {
            return Ok(found);
        }
        let mut responsive = Vec::new();
        for identifier in found {
            match self
                .call(
                    "callAndWait",
                    &[
                        json!(identifier),
                        json!({"samp.mtype":"samp.app.ping","samp.params":{}}),
                        json!("1"),
                    ],
                )
                .await
            {
                Ok(reply) if reply["samp.status"] == "samp.ok" => responsive.push(identifier),
                Ok(_) => {}
                Err(error) => eprintln!("NightBloom Aladin ping: {error:#}"),
            }
        }
        Ok(responsive)
    }

    async fn recipient(&self, mtype: &str) -> Result<String> {
        let recipients = self.aladin(mtype, false).await?;
        ensure!(
            recipients.len() == 1,
            "Expected one SAMP-connected Aladin; found {}. Connect exactly one Aladin using Connect with SAMP.",
            recipients.len()
        );
        Ok(recipients[0].clone())
    }

    async fn finish<T>(&self, result: Result<T>) -> Result<T> {
        if let Err(error) = self.call("unregister", &[]).await {
            eprintln!("NightBloom SAMP disconnect: {error:#}");
        }
        result
    }
}

impl Samp {
    pub async fn ping(&self) -> Result<bool> {
        let client = SampClient::connect(self.path()?).await?;
        let result = client
            .aladin("snapshot.get.jpg", true)
            .await
            .map(|clients| !clients.is_empty());
        client.finish(result).await
    }

    pub async fn snapshot(&self) -> Result<String> {
        let client = SampClient::connect(self.path()?).await?;
        let result = async {
            let recipient = client.recipient("snapshot.get.jpg").await?;
            let reply = client
                .call(
                    "callAndWait",
                    &[
                        json!(recipient),
                        json!({"samp.mtype":"snapshot.get.jpg","samp.params":{}}),
                        json!("10"),
                    ],
                )
                .await?;
            ensure!(
                reply["samp.status"] == "samp.ok",
                "Aladin could not capture the current view."
            );
            reply["samp.result"]["data"]
                .as_str()
                .map(str::to_owned)
                .context("Aladin snapshot response is missing its image data.")
        }
        .await;
        client.finish(result).await
    }

    pub async fn scripts(&self, commands: &[String]) -> Result<()> {
        let client = SampClient::connect(self.path()?).await?;
        let result = async {
            let recipient = client.recipient("script.aladin.send").await?;
            for (index, script) in commands.iter().enumerate() {
                if index > 0 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                client
                .call(
                    "notify",
                    &[
                        json!(recipient),
                        json!({"samp.mtype":"script.aladin.send","samp.params":{"script":script}}),
                    ],
                )
                .await?;
            }
            Ok(())
        }
        .await;
        client.finish(result).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn xml_rpc_preserves_nested_values_and_rejects_faults_and_remote_hubs() {
        let value = json!({"name":"<Night & Bloom> \"test\"","array":["x",{"count":3}],"ok":true});
        let xml = format!(
            "<methodResponse><params><param>{}</param></params></methodResponse>",
            encode(&value).unwrap()
        );
        assert_eq!(response(&xml).unwrap(), value);
        assert!(
            response("<methodResponse><fault><value><string>private</string></value></fault></methodResponse>")
                .unwrap_err().to_string().contains("rejected")
        );
        assert!(local_url("http://127.0.0.1:2525/xmlrpc").is_ok());
        assert!(local_url("http://[::1]:2525/xmlrpc").is_ok());
        assert!(local_url("https://example.com").is_err());
        assert!(local_url("http://secret@localhost").is_err());
        assert!(
            response("<!DOCTYPE x [<!ENTITY x 'test'>]><methodResponse>&x;</methodResponse>")
                .is_err()
        );
    }

    #[tokio::test]
    async fn samp_registration_detects_live_aladin_and_unregisters() {
        use axum::{Router, body::Bytes, extract::State, routing::post};
        async fn hub(State(unregistered): State<Arc<AtomicUsize>>, bytes: Bytes) -> String {
            let xml = std::str::from_utf8(&bytes).unwrap();
            let document = Document::parse(xml).unwrap();
            let root = document.root_element();
            let method = child(root, "methodName").unwrap().text().unwrap();
            let params: Vec<_> = child(root, "params")
                .unwrap()
                .children()
                .filter(|n| n.has_tag_name("param"))
                .map(|node| decode(child(node, "value").unwrap(), 0).unwrap())
                .collect();
            let result = match method {
                "samp.hub.register" => {
                    assert_eq!(params[0], "test-secret");
                    json!({"samp.private-key":"test-key"})
                }
                "samp.hub.getSubscribedClients" => json!({"live":{},"stale":{},"other":{}}),
                "samp.hub.getMetadata" => json!({
                    "samp.name":if params[1]=="other" {"Other application"} else {"Aladin desktop"}
                }),
                "samp.hub.callAndWait" => json!({
                    "samp.status":if params[1]=="stale" {"samp.error"} else {"samp.ok"}
                }),
                "samp.hub.unregister" => {
                    unregistered.fetch_add(1, Ordering::SeqCst);
                    json!("")
                }
                "samp.hub.declareMetadata" => json!(""),
                _ => panic!("Unexpected SAMP method"),
            };
            format!(
                "<methodResponse><params><param>{}</param></params></methodResponse>",
                encode(&result).unwrap()
            )
        }
        let unregistered = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/", post(hub))
            .with_state(unregistered.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".samp");
        std::fs::write(
            &path,
            format!("samp.secret=test-secret\nsamp.hub.xmlrpc.url=http://{address}/\n"),
        )
        .unwrap();
        let client = SampClient::connect(path).await.unwrap();
        let result = client.aladin("snapshot.get.jpg", true).await.unwrap();
        assert_eq!(result, ["live"]);
        assert_eq!(client.recipient("snapshot.get.jpg").await.unwrap(), "live");
        client.finish(Ok(())).await.unwrap();
        assert_eq!(unregistered.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
