#![allow(dead_code)]

//! Минимальный клиент UPnP IGD (Internet Gateway Device).
//!
//! Зачем: nanocord соединяет два компьютера напрямую по TCP. Если они в разных
//! сетях, пакету нужно пробиться через NAT роутера, а проброс порта руками умеют
//! делать не все. UPnP IGD позволяет попросить роутер открыть порт программно.
//!
//! Реализовано на `std` без зависимостей: SSDP-обнаружение шлюза, чтение его
//! описания и два SOAP-вызова (AddPortMapping / DeletePortMapping). HTTPS не
//! нужен — IGD-описания отдаются по обычному HTTP в локальной сети.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

const SSDP_ADDR: &str = "239.255.255.250:1900";

/// Найденный шлюз: куда слать SOAP и от чьего имени.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gateway {
    /// Полный URL control-эндпоинта, например http://192.168.0.1:5000/ctl/IPConn
    pub control_url: String,
    /// Тип сервиса, он обязан совпадать с namespace в SOAP-запросе.
    pub service_type: String,
}

/// Результат проброса.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub external_ip: String,
    pub external_port: u16,
    pub internal_port: u16,
}

// ---------------------------------------------------------------- HTTP

fn parse_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(pos) => (&rest[..pos], &rest[pos..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().ok()?),
        None => (authority.to_string(), 80),
    };
    Some((host, port, path.to_string()))
}

/// Один HTTP/1.1 запрос «до закрытия соединения». Возвращает тело ответа.
fn http_request(
    url: &str,
    method: &str,
    extra_headers: &[(&str, &str)],
    body: &str,
    timeout: Duration,
) -> Result<String, String> {
    let (host, port, path) = parse_url(url).ok_or_else(|| format!("плохой URL: {url}"))?;
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|_| format!("не разобрать адрес {host}:{port}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| format!("нет соединения с {addr}: {e}"))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in extra_headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(body);

    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("ошибка отправки: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("ошибка чтения: {e}"))?;
    let text = String::from_utf8_lossy(&raw).to_string();

    // 200 ожидаем, но тело нужно и при 500 — там лежит текст ошибки UPnP.
    match text.split_once("\r\n\r\n") {
        Some((_, body)) => Ok(body.to_string()),
        None => Ok(text),
    }
}

// ---------------------------------------------------------------- SSDP

/// Достаёт LOCATION из SSDP-ответа.
fn parse_ssdp_location(response: &str) -> Option<String> {
    for line in response.lines() {
        let line = line.trim();
        if line.len() > 9 && line[..9].eq_ignore_ascii_case("location:") {
            let value = line[9..].trim();
            if value.starts_with("http://") {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Просит роутер представиться по SSDP. Возвращает URL его описания.
fn ssdp_discover(timeout: Duration) -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));

    // :1 и :2 — разные версии IGD; многие роутеры отвечают только на одну.
    for service in [
        "urn:schemas-upnp-org:device:InternetGatewayDevice:1",
        "urn:schemas-upnp-org:device:InternetGatewayDevice:2",
        "ssdp:all",
    ] {
        let request = format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_ADDR}\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: {service}\r\n\r\n"
        );
        if socket.send_to(request.as_bytes(), SSDP_ADDR).is_err() {
            continue;
        }

        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 4096];
        while Instant::now() < deadline {
            match socket.recv_from(&mut buf) {
                Ok((n, _)) => {
                    let text = String::from_utf8_lossy(&buf[..n]).to_string();
                    if let Some(location) = parse_ssdp_location(&text) {
                        return Some(location);
                    }
                }
                Err(_) => break, // таймаут — пробуем следующий ST
            }
        }
    }
    None
}

// ---------------------------------------------------------------- Описание

/// Вытаскивает из XML описания устройства URL управления для WAN-подключения.
///
/// Нужны оба типа: `WANIPConnection` (обычный NAT) и `WANPPPConnection`
/// (PPPoE-подключение, типичное для домашних роутеров с ppp0).
fn parse_control_url(xml: &str) -> Option<Gateway> {
    let base = extract_tag_text(xml, "URLBase").map(|b| b.trim_end_matches('/').to_string());

    for wanted in [
        "urn:schemas-upnp-org:service:WANPPPConnection:1",
        "urn:schemas-upnp-org:service:WANIPConnection:1",
        "urn:schemas-upnp-org:service:WANIPConnection:2",
        "urn:schemas-upnp-org:service:WANPPPConnection:2",
    ] {
        // Сервис описан блоком <service>: serviceType + controlURL внутри.
        // Блоков в описании несколько (Layer3Forwarding, WANCommonInterfaceConfig,
        // WANDSLLinkConfig, WANIPConnection), и нужный обычно не первый — поэтому
        // идём по всему документу, а не только по первому блоку.
        let close_tag = "</service>";
        let mut rest = xml;
        while let Some(start) = rest.find("<service>") {
            let after = &rest[start..];
            let end = after
                .find(close_tag)
                .map(|pos| pos + close_tag.len())
                .unwrap_or(after.len());
            let block = &after[..end];

            if let Some(service_type) = extract_tag_text(block, "serviceType") {
                if service_type.trim() == wanted {
                    if let Some(control) = extract_tag_text(block, "controlURL") {
                        let control = control.trim();
                        let url = if control.starts_with("http://") {
                            control.to_string()
                        } else if control.starts_with('/') {
                            match &base {
                                Some(base) => format!("{base}{control}"),
                                None => control.to_string(),
                            }
                        } else {
                            match &base {
                                Some(base) => format!("{base}/{control}"),
                                None => control.to_string(),
                            }
                        };
                        return Some(Gateway {
                            control_url: url,
                            service_type: wanted.to_string(),
                        });
                    }
                }
            }
            rest = &after[end..];
        }
    }
    None
}

fn extract_tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

/// Достаёт значение из ответа вида `<NewExternalIPAddress>1.2.3.4</NewExternalIPAddress>`.
fn extract_value(xml: &str, tag: &str) -> Option<String> {
    extract_tag_text(xml, tag).map(|v| v.trim().to_string())
}

// ---------------------------------------------------------------- SOAP

fn soap(
    gateway: &Gateway,
    action: &str,
    args: &str,
    timeout: Duration,
) -> Result<String, String> {
    let body = format!(
        "<?xml version=\"1.0\"?>\r\n\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <s:Body>\
         <u:{action} xmlns:u=\"{service}\">{args}</u:{action}>\
         </s:Body></s:Envelope>",
        service = gateway.service_type
    );
    let soap_action = format!("\"{}#{action}\"", gateway.service_type);
    http_request(
        &gateway.control_url,
        "POST",
        &[
            ("Content-Type", "text/xml; charset=\"utf-8\""),
            ("SOAPAction", &soap_action),
        ],
        &body,
        timeout,
    )
}

/// Порты, на которых домашние роутеры держат описание IGD.
/// 49152 — стандарт TR-064 и самый частый; остальные — запасные варианты.
const IGD_PORTS: [u16; 5] = [49152, 5000, 2869, 80, 8080];

/// Кандидаты в адрес шлюза, когда SSDP молчит.
///
/// Мультикаст нередко режется (Windows Firewall, изоляция клиентов на Wi-Fi,
/// IGMP snooping), но само HTTP-описание по адресу шлюза доступно. Адрес берём
/// из подсети локального интерфейса: .1 и .254 — почти все домашние роутеры.
fn candidate_gateways() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    if let Some(ip) = local_ipv4() {
        let o = ip.octets();
        for host in [1u8, 254u8] {
            let candidate = Ipv4Addr::new(o[0], o[1], o[2], host);
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }
    for fallback in [Ipv4Addr::new(192, 168, 0, 1), Ipv4Addr::new(192, 168, 1, 1)] {
        if !out.contains(&fallback) {
            out.push(fallback);
        }
    }
    out
}

/// Ищет IGD по HTTP, без SSDP.
fn http_discover(timeout: Duration) -> Option<Gateway> {
    for gateway_ip in candidate_gateways() {
        for port in IGD_PORTS {
            let url = format!("http://{gateway_ip}:{port}/rootDesc.xml");
            let Ok(xml) = http_request(&url, "GET", &[], "", timeout) else {
                continue;
            };
            if let Some(gateway) = parse_control_url(&xml) {
                return Some(gateway);
            }
        }
    }
    None
}

/// Находит шлюз и читает его описание.
pub fn discover(timeout: Duration) -> Result<Gateway, String> {
    if let Some(location) = ssdp_discover(timeout) {
        let xml = http_request(&location, "GET", &[], "", timeout)
            .map_err(|e| format!("описание шлюза недоступно: {e}"))?;
        if let Some(gateway) = parse_control_url(&xml) {
            return Ok(gateway);
        }
    }

    // SSDP молчит — пробуем HTTP напрямую. Без этого на сетях, где режется
    // мультикаст, UPnP считался «выключенным», хотя роутер его поддерживает.
    http_discover(timeout).ok_or_else(|| {
        "шлюз UPnP не найден ни по SSDP, ни по HTTP (UPnP выключен на роутере или сеть его блокирует)"
            .to_string()
    })
}

/// Внешний IP по данным роутера (без обращения к внешним сервисам).
pub fn external_ip(gateway: &Gateway, timeout: Duration) -> Result<String, String> {
    let reply = soap(gateway, "GetExternalIPAddress", "", timeout)?;
    extract_value(&reply, "NewExternalIPAddress")
        .filter(|ip| !ip.is_empty() && ip != "0.0.0.0")
        .ok_or_else(|| "роутер не сообщил внешний адрес".to_string())
}

/// Просит роутер пробросить TCP-порт на эту машину.
///
/// `lease` в секундах: 0 — бессрочно (так делают многие домашние роутеры).
pub fn add_tcp_mapping(
    gateway: &Gateway,
    internal_ip: Ipv4Addr,
    port: u16,
    description: &str,
    lease: u32,
    timeout: Duration,
) -> Result<(), String> {
    let args = format!(
        "<NewRemoteHost></NewRemoteHost>\
         <NewExternalPort>{port}</NewExternalPort>\
         <NewProtocol>TCP</NewProtocol>\
         <NewInternalPort>{port}</NewInternalPort>\
         <NewInternalClient>{internal_ip}</NewInternalClient>\
         <NewEnabled>1</NewEnabled>\
         <NewPortMappingDescription>{description}</NewPortMappingDescription>\
         <NewLeaseDuration>{lease}</NewLeaseDuration>"
    );
    let reply = soap(gateway, "AddPortMapping", &args, timeout)?;
    // Успех — пустое тело; ошибка — UPnPError с кодом (718 ConflictInMappingEntry,
    // 724 SamePortValuesRequired и т.п.). Код полезен в логе.
    if reply.contains("<s:Fault>") || reply.contains("UPnPError") {
        let code = extract_value(&reply, "errorCode").unwrap_or_else(|| "?".to_string());
        let desc = extract_value(&reply, "errorDescription").unwrap_or_default();
        return Err(format!("роутер отказал (код {code}: {desc})"));
    }
    Ok(())
}

/// Убирает проброс (best-effort при остановке сервера).
pub fn delete_tcp_mapping(
    gateway: &Gateway,
    port: u16,
    timeout: Duration,
) -> Result<(), String> {
    let args = format!(
        "<NewRemoteHost></NewRemoteHost>\
         <NewExternalPort>{port}</NewExternalPort>\
         <NewProtocol>TCP</NewProtocol>"
    );
    let reply = soap(gateway, "DeletePortMapping", &args, timeout)?;
    if reply.contains("<s:Fault>") || reply.contains("UPnPError") {
        let code = extract_value(&reply, "errorCode").unwrap_or_else(|| "?".to_string());
        return Err(format!("роутер не удалил проброс (код {code})"));
    }
    Ok(())
}

/// Локальный IPv4, с которого видно сеть по умолчанию.
///
/// Трюк с UDP: подключение не отправляет пакетов, но ядро выбирает исходящий
/// интерфейс — так получается адрес, который роутер поймёт.
pub fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(addr) if !addr.ip().is_unspecified() => Some(*addr.ip()),
        _ => None,
    }
}

/// Полный сценарий: найти шлюз, запросить внешний адрес и открыть порт.
pub fn ensure_mapping(port: u16, label: &str) -> Result<Mapping, String> {
    let timeout = Duration::from_secs(3);
    let gateway = discover(timeout)?;
    let internal = local_ipv4().ok_or_else(|| "не удалось определить локальный IPv4".to_string())?;
    let external_ip = external_ip(&gateway, timeout).unwrap_or_else(|_| "?".to_string());

    // Сначала просим бессрочный проброс; если роутер требует аренду — 2 часа.
    if let Err(first) = add_tcp_mapping(&gateway, internal, port, label, 0, timeout) {
        add_tcp_mapping(&gateway, internal, port, label, 7200, timeout)
            .map_err(|e| format!("{first}; повтор с арендой 2 ч: {e}"))?;
    }

    Ok(Mapping {
        external_ip,
        external_port: port,
        internal_port: port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_is_parsed_case_insensitively() {
        let response = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nlocation: http://192.168.0.1:5000/rootDesc.xml\r\nST: upnp:rootdevice\r\n\r\n";
        assert_eq!(
            parse_ssdp_location(response).as_deref(),
            Some("http://192.168.0.1:5000/rootDesc.xml")
        );
    }

    #[test]
    fn ssdp_without_location_returns_none() {
        assert_eq!(parse_ssdp_location("HTTP/1.1 200 OK\r\nST: ssdp:all\r\n\r\n"), None);
    }

    #[test]
    fn ppp_connection_with_absolute_urlbase() {
        let xml = r#"<root><URLBase>http://192.168.0.1:5000/</URLBase>
            <service>
              <serviceType>urn:schemas-upnp-org:service:WANPPPConnection:1</serviceType>
              <controlURL>/ctl/IPConn</controlURL>
            </service></root>"#;
        let gateway = parse_control_url(xml).expect("должен найтись PPP-сервис");
        assert_eq!(gateway.control_url, "http://192.168.0.1:5000/ctl/IPConn");
        assert_eq!(
            gateway.service_type,
            "urn:schemas-upnp-org:service:WANPPPConnection:1"
        );
    }

    #[test]
    fn wan_service_is_found_when_it_is_not_the_first_block() {
        // Так выглядит реальное описание GPON-роутера: WANIPConnection идёт
        // четвёртым, до него лежат три других сервиса.
        let xml = r#"<root><URLBase>http://192.168.0.1:49152</URLBase><device><serviceList>
            <service><serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1</serviceType><controlURL>/ctl/L3F</controlURL></service>
            <service><serviceType>urn:schemas-upnp-org:service:WANCommonInterfaceConfig:1</serviceType><controlURL>/ctl/CmnIfCfg</controlURL></service>
            <service><serviceType>urn:schemas-upnp-org:service:WANDSLLinkConfig:1</serviceType><controlURL>/ctl/WanDslLk</controlURL></service>
            <service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType><controlURL>/ctl/IPConn</controlURL></service>
            </serviceList></device></root>"#;
        let gateway = parse_control_url(xml).expect("WANIPConnection должен найтись не в первом блоке");
        assert_eq!(gateway.control_url, "http://192.168.0.1:49152/ctl/IPConn");
    }

    #[test]
    fn ip_connection_with_relative_url_without_urlbase() {
        let xml = r#"<service>
              <serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
              <controlURL>ctl/IPConn</controlURL>
            </service>"#;
        let gateway = parse_control_url(xml).expect("должен найтись IP-сервис");
        assert_eq!(gateway.control_url, "ctl/IPConn");
    }

    #[test]
    fn external_ip_is_extracted() {
        let body = "<NewExternalIPAddress>88.204.56.149</NewExternalIPAddress>";
        assert_eq!(
            extract_value(body, "NewExternalIPAddress").as_deref(),
            Some("88.204.56.149")
        );
    }

    #[test]
    fn url_parsing_handles_port_and_path() {
        assert_eq!(
            parse_url("http://192.168.0.1:5000/ctl/IPConn"),
            Some(("192.168.0.1".to_string(), 5000, "/ctl/IPConn".to_string()))
        );
        assert_eq!(
            parse_url("http://gateway/desc.xml"),
            Some(("gateway".to_string(), 80, "/desc.xml".to_string()))
        );
    }
}
