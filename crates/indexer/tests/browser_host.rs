use indexer::browser_host::{group_key, normalize_host};

#[test]
fn normalize_host_lowercases_strips_trailing_dot_and_brackets() {
    assert_eq!(normalize_host("Example.ORG.").unwrap(), "example.org");
    assert_eq!(normalize_host("[::1]").unwrap(), "::1");
}

#[test]
fn normalize_host_canonicalizes_ip_literals() {
    assert_eq!(normalize_host("[2001:DB8:0:0::1]").unwrap(), "2001:db8::1");
    assert_eq!(normalize_host("2001:db8::1").unwrap(), "2001:db8::1");
    assert_eq!(normalize_host("10.0.0.1.").unwrap(), "10.0.0.1");
    assert_eq!(group_key("2001:db8::1"), "2001:db8::1");
    assert_eq!(group_key("10.0.0.1"), "10.0.0.1");
}

#[test]
fn normalize_host_converts_idn_to_punycode() {
    assert_eq!(normalize_host("例子.中国").unwrap(), "xn--fsqu00a.xn--fiqs8s");
}

#[test]
fn group_key_cases() {
    // Same group
    assert_eq!(group_key("a.example.org"), group_key("b.example.org"));
    assert_eq!(group_key("a.example.org"), group_key("example.org"));
    assert_eq!(group_key("a.b.co.uk"), group_key("c.b.co.uk"));
    assert_eq!(group_key("a.b.co.uk"), group_key("b.co.uk"));
    assert_eq!(group_key("x.a.example.org"), group_key("y.b.example.org"));
    assert_eq!(group_key("tracker.example.org"), group_key("x.forum.example.org"));
    assert_eq!(group_key("a.example.com.cn"), group_key("x.b.example.com.cn"));
    assert_eq!(group_key("Example.ORG"), group_key("example.org"));
    assert_eq!(group_key("example.org."), group_key("example.org"));
    assert_eq!(group_key("例子.中国"), group_key("xn--fsqu00a.xn--fiqs8s"));
    assert_eq!(group_key("localhost"), group_key("foo.localhost"));

    // Different group
    assert_ne!(group_key("example.org"), group_key("other.org"));
    assert_ne!(group_key("x.co.uk"), group_key("y.co.uk"));
    assert_ne!(group_key("xx.com.cn"), group_key("yy.com.cn"));
    assert_ne!(group_key("a.xx.com.cn"), group_key("b.yy.com.cn"));
    assert_ne!(group_key("10.0.0.1"), group_key("10.0.0.2"));
    assert_ne!(group_key("1.2.3.4"), group_key("5.2.3.4"));
    assert_ne!(group_key("jackett"), group_key("prowlarr"));
}

#[test]
fn normalize_host_is_independent_of_cookie_jar() {
    assert_eq!(normalize_host("Example.ORG").unwrap(), "example.org");
}
