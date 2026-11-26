// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(dead_code)]

//! A small writer of Apple keyed archives (binary property lists) shaped like QLab 5 workspace
//! files, for tests that need no QLab-saved file. Objects can be shared, so a small file can
//! stand for a hostile one.

use std::collections::HashMap;

/// A value in an archived object's fields: a reference to an archived object, or a number
/// stored inline.
#[derive(Clone, Copy, Debug)]
pub enum V {
    Ref(u64),
    Int(i64),
    Real(f64),
    Bool(bool),
}

/// One property-list object.
enum P {
    Str(String),
    Int(i64),
    Real(f64),
    Bool(bool),
    Data(Vec<u8>),
    Uid(u64),
    Array(Vec<usize>),
    Dict(Vec<(usize, usize)>),
}

/// A keyed archive being written: `$objects[0]` is `$null`.
pub struct Archive {
    plist: Vec<P>,
    objects: Vec<usize>,
    keys: HashMap<String, usize>,
    classes: HashMap<String, V>,
}

impl Default for Archive {
    fn default() -> Self {
        Self::new()
    }
}

impl Archive {
    pub fn new() -> Self {
        let mut a = Self {
            plist: Vec::new(),
            objects: Vec::new(),
            keys: HashMap::new(),
            classes: HashMap::new(),
        };
        a.push(P::Str("$null".into()));
        a
    }

    fn add(&mut self, p: P) -> usize {
        self.plist.push(p);
        self.plist.len() - 1
    }

    fn push(&mut self, p: P) -> V {
        let at = self.add(p);
        self.objects.push(at);
        V::Ref(self.objects.len() as u64 - 1)
    }

    /// A plain string object, shared by every key spelled the same.
    fn key(&mut self, k: &str) -> usize {
        if let Some(at) = self.keys.get(k) {
            return *at;
        }
        let at = self.add(P::Str(k.into()));
        self.keys.insert(k.into(), at);
        at
    }

    fn value(&mut self, v: V) -> usize {
        self.add(match v {
            V::Ref(u) => P::Uid(u),
            V::Int(n) => P::Int(n),
            V::Real(r) => P::Real(r),
            V::Bool(b) => P::Bool(b),
        })
    }

    pub fn null() -> V {
        V::Ref(0)
    }

    /// A reference to a made-up object past the end of `$objects`: damaged data.
    pub fn dangling() -> V {
        V::Ref(u64::from(u32::MAX))
    }

    pub fn string(&mut self, s: &str) -> V {
        self.push(P::Str(s.into()))
    }

    pub fn data(&mut self, bytes: Vec<u8>) -> V {
        self.push(P::Data(bytes))
    }

    fn class(&mut self, name: &str) -> V {
        if let Some(v) = self.classes.get(name) {
            return *v;
        }
        let (n, base) = (self.key(name), self.key("NSObject"));
        let classes = self.add(P::Array(vec![n, base]));
        let (k1, k2) = (self.key("$classname"), self.key("$classes"));
        let v = self.push(P::Dict(vec![(k1, n), (k2, classes)]));
        self.classes.insert(name.into(), v);
        v
    }

    fn object_raw(&mut self, class: &str, mut fields: Vec<(usize, usize)>) -> V {
        let c = self.class(class);
        let key = self.key("$class");
        let c = self.value(c);
        fields.push((key, c));
        self.push(P::Dict(fields))
    }

    /// An archived object of class `class` with `fields`.
    pub fn object(&mut self, class: &str, fields: &[(&str, V)]) -> V {
        let fields = fields
            .iter()
            .map(|(k, v)| (self.key(k), self.value(*v)))
            .collect();
        self.object_raw(class, fields)
    }

    fn refs(&mut self, items: &[V]) -> usize {
        let refs = items.iter().map(|v| self.value(*v)).collect();
        self.add(P::Array(refs))
    }

    /// An `NSArray` of `items`.
    pub fn array(&mut self, items: &[V]) -> V {
        let (key, refs) = (self.key("NS.objects"), self.refs(items));
        self.object_raw("NSMutableArray", vec![(key, refs)])
    }

    /// An `NSDictionary` of `pairs`.
    pub fn dict(&mut self, pairs: &[(V, V)]) -> V {
        let keys: Vec<V> = pairs.iter().map(|(k, _)| *k).collect();
        let values: Vec<V> = pairs.iter().map(|(_, v)| *v).collect();
        let (k1, k2) = (self.key("NS.keys"), self.key("NS.objects"));
        let (keys, values) = (self.refs(&keys), self.refs(&values));
        self.object_raw("NSMutableDictionary", vec![(k1, keys), (k2, values)])
    }

    /// An `NSDictionary` with string keys.
    pub fn dict_str(&mut self, pairs: &[(&str, V)]) -> V {
        let pairs: Vec<(V, V)> = pairs.iter().map(|(k, v)| (self.string(k), *v)).collect();
        self.dict(&pairs)
    }

    /// The archive's bytes, with `root` as `$top.root`.
    pub fn finish(mut self, root: V) -> Vec<u8> {
        let objects = self.add(P::Array(self.objects.clone()));
        let (k_root, root) = (self.key("root"), self.value(root));
        let top = self.add(P::Dict(vec![(k_root, root)]));
        let archiver = self.add(P::Str("NSKeyedArchiver".into()));
        let version = self.add(P::Int(100_000));
        let keys = ["$archiver", "$objects", "$top", "$version"].map(|k| self.key(k));
        let top = self.add(P::Dict(
            keys.into_iter()
                .zip([archiver, objects, top, version])
                .collect(),
        ));
        write_bplist(&self.plist, top)
    }
}

fn marker(out: &mut Vec<u8>, kind: u8, len: usize) {
    if len < 15 {
        out.push(kind << 4 | len as u8);
    } else {
        out.push(kind << 4 | 0xf);
        out.push(0x13);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
}

/// A `bplist00` of `objects` with four-byte references and eight-byte offsets.
fn write_bplist(objects: &[P], top: usize) -> Vec<u8> {
    let r = |out: &mut Vec<u8>, i: usize| out.extend_from_slice(&(i as u32).to_be_bytes());
    let mut out = b"bplist00".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for p in objects {
        offsets.push(out.len() as u64);
        match p {
            P::Str(s) if s.is_ascii() => {
                marker(&mut out, 0x5, s.len());
                out.extend_from_slice(s.as_bytes());
            }
            P::Str(s) => {
                let units: Vec<u16> = s.encode_utf16().collect();
                marker(&mut out, 0x6, units.len());
                units
                    .iter()
                    .for_each(|u| out.extend_from_slice(&u.to_be_bytes()));
            }
            P::Int(n) => {
                out.push(0x13);
                out.extend_from_slice(&n.to_be_bytes());
            }
            P::Real(x) => {
                out.push(0x23);
                out.extend_from_slice(&x.to_bits().to_be_bytes());
            }
            P::Bool(b) => out.push(if *b { 0x09 } else { 0x08 }),
            P::Data(d) => {
                marker(&mut out, 0x4, d.len());
                out.extend_from_slice(d);
            }
            P::Uid(u) => {
                out.push(0x83);
                out.extend_from_slice(&(*u as u32).to_be_bytes());
            }
            P::Array(items) => {
                marker(&mut out, 0xa, items.len());
                items.iter().for_each(|i| r(&mut out, *i));
            }
            P::Dict(pairs) => {
                marker(&mut out, 0xd, pairs.len());
                pairs.iter().for_each(|(k, _)| r(&mut out, *k));
                pairs.iter().for_each(|(_, v)| r(&mut out, *v));
            }
        }
    }
    let table = out.len() as u64;
    offsets
        .iter()
        .for_each(|o| out.extend_from_slice(&o.to_be_bytes()));
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 8, 4]);
    out.extend_from_slice(&(objects.len() as u64).to_be_bytes());
    out.extend_from_slice(&(top as u64).to_be_bytes());
    out.extend_from_slice(&table.to_be_bytes());
    out
}

/// A QLab 5 workspace file named `name`: `top` holds `settings` (its `settings` dictionary, or
/// none), `cues` holds the cue objects of `lists`, each a cue list's name and cues.
pub fn qlab_file(
    name: &str,
    mut top: Archive,
    settings: Option<V>,
    mut cues: Archive,
    lists: Vec<(&str, Vec<V>)>,
) -> Vec<u8> {
    let mut list_objects = Vec::new();
    for (i, (list, members)) in lists.into_iter().enumerate() {
        let id = cues.string(&format!("list-{i}"));
        let list = cues.string(list);
        let members = cues.array(&members);
        list_objects.push(cues.object(
            "GroupCue",
            &[
                ("uniqueID", id),
                ("name", list),
                ("groupMode", V::Int(0)),
                ("cues", members),
            ],
        ));
    }
    let all = cues.array(&list_objects);
    let root = cues.object("GroupCue", &[("groupMode", V::Int(3)), ("cues", all)]);
    let inner = cues.finish(root);
    let lists = top.data(inner);
    let name = top.string(&format!("{name}.qlab5"));
    let version = top.string("5.6.5");
    let mut pairs = vec![
        ("workspaceName", name),
        ("QLabShortVersionString", version),
        ("cueLists", lists),
    ];
    if let Some(s) = settings {
        pairs.push(("settings", s));
    }
    let root = top.dict_str(&pairs);
    top.finish(root)
}

/// A cue of class `class` with unique id `id` and `fields`.
pub fn cue(a: &mut Archive, class: &str, id: &str, fields: &[(&str, V)]) -> V {
    let id = a.string(id);
    let mut all = vec![("uniqueID", id)];
    all.extend_from_slice(fields);
    a.object(class, &all)
}
