import os
os.chdir(r"C:\work\wwd")


def patch(p, pairs, append=None):
    s = open(p, encoding="utf-8").read()
    for a, b in pairs:
        if a not in s:
            raise SystemExit(f"{p}: anchor not found: {a[:90]}")
        s = s.replace(a, b, 1)
    if append:
        s += append
    open(p, "w", encoding="utf-8").write(s)


patch("crates/wwd-linux/src/net.rs", [], append=open("scripts/patches/net_dns.rs.txt", encoding="utf-8").read())
patch("crates/wwd-linux/src/fs.rs", [
    ("""    EventFd(std::sync::Arc<crate::pipe::EventFd>),
}""", """    EventFd(std::sync::Arc<crate::pipe::EventFd>),
    /// netd dnsproxyd connection: request bytes in, reply bytes out
    Dns { req: Vec<u8>, resp: Vec<u8>, pos: usize },
}"""),
    ("""            "/dev/__properties__" => return Resolved::Special("properties"),""", """            "/dev/__properties__" => return Resolved::Special("properties"),
            "/dev/socket/dnsproxyd" => return Resolved::Special("dnsproxyd"),"""),
    ("""            "properties" => Node::Mem {""", """            "dnsproxyd" => Node::Dns { req: Vec::new(), resp: Vec::new(), pos: 0 },
            "properties" => Node::Mem {"""),
    ("""        Node::Dir { .. } => -EISDIR,
        Node::Null => 0,""", """        Node::Dir { .. } => -EISDIR,
        Node::Dns { resp, pos, .. } => {
            let k = buf.len().min(resp.len() - *pos);
            buf[..k].copy_from_slice(&resp[*pos..*pos + k]);
            *pos += k;
            k as i64
        }
        Node::Null => 0,"""),
    ("""        Node::Null | Node::Zero | Node::Random => buf.len() as i64,
        Node::Mem { .. } => -EBADF,""", """        Node::Null | Node::Zero | Node::Random => buf.len() as i64,
        Node::Dns { req, resp, pos } => {
            req.extend_from_slice(buf);
            while let Some(z) = req.iter().position(|&b| b == 0) {
                let q: Vec<u8> = req.drain(..=z).collect();
                let text = String::from_utf8_lossy(&q[..z]).into_owned();
                resp.drain(..*pos);
                *pos = 0;
                resp.extend(crate::net::dns_query(&text));
            }
            buf.len() as i64
        }
        Node::Mem { .. } => -EBADF,"""),
])
print("patched")
