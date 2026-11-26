// SPDX-License-Identifier: GPL-3.0-or-later

//! Apple keyed archives (`NSKeyedArchiver`) over a [`Plist`]: `$top.root` is the root object,
//! `$objects` holds every object, and archived objects refer to each other by `Uid`. Objects
//! are read lazily, so a cycle in the file costs reads from the budget, never a loop: callers
//! walk only the shapes they expect, with depth bounds of their own. Lists are bounded
//! ([`MAX_FIELDS`], [`MAX_ITEMS`]) and charged to the import's budget.

use super::QlabError;
use super::plist::{Budget, Plist, Refs, Value, is_ascii, text};

/// Most keyed archives (the file and those nested in it as data) one import opens.
pub(super) const MAX_ARCHIVES: u32 = 4;

/// Most fields of one archived object (a QLab cue has about a hundred).
pub(super) const MAX_FIELDS: usize = 1024;

/// Most members of one `NSArray`, `NSSet` or `NSDictionary` (a full level matrix has about
/// 17,000 cells).
pub(super) const MAX_ITEMS: usize = 65_536;

/// Longest dictionary key kept, in bytes; an entry with a longer key is skipped unread (QLab's
/// keys are field names, ids and cell numbers).
pub(super) const MAX_KEY_BYTES: usize = 1024;

/// An opened keyed archive.
pub(super) struct Archive<'a> {
    plist: Plist<'a>,
    objects: Refs<'a>,
    root: Value<'a>,
}

/// An archived object's own fields (keys and raw values), read once so lookups need no reads.
pub(super) struct Fields<'a>(Vec<(&'a [u8], Value<'a>)>);

impl<'a> Fields<'a> {
    pub(super) fn get(&self, key: &str) -> Option<Value<'a>> {
        self.0
            .iter()
            .find(|(k, _)| *k == key.as_bytes())
            .map(|(_, v)| *v)
    }
}

impl<'a> Archive<'a> {
    /// Opens a keyed archive; `opened` counts the archives of this import.
    pub(super) fn open(
        data: &'a [u8],
        budget: &'a Budget,
        opened: &mut u32,
    ) -> Result<Self, QlabError> {
        *opened += 1;
        if *opened > MAX_ARCHIVES {
            return Err(QlabError::TooLarge("nested archives"));
        }
        let plist = Plist::open(data, budget)?;
        let top = plist.object(plist.top())?;
        let Value::Dict { keys, values } = top else {
            return Err(QlabError::NotKeyedArchive);
        };
        let mut archiver = false;
        let mut objects = None;
        let mut top_dict = None;
        for (k, v) in keys.iter().zip(values.iter()) {
            let key = plist.object(k)?;
            if is_ascii(key, "$archiver") {
                archiver = is_ascii(plist.object(v)?, "NSKeyedArchiver");
            } else if is_ascii(key, "$objects") {
                objects = Some(plist.object(v)?);
            } else if is_ascii(key, "$top") {
                top_dict = Some(plist.object(v)?);
            }
        }
        let (true, Some(Value::Array(objects)), Some(Value::Dict { keys, values })) =
            (archiver, objects, top_dict)
        else {
            return Err(QlabError::NotKeyedArchive);
        };
        let mut root = None;
        for (k, v) in keys.iter().zip(values.iter()) {
            if is_ascii(plist.object(k)?, "root") {
                root = Some(plist.object(v)?);
            }
        }
        let root = root.ok_or(QlabError::NotKeyedArchive)?;
        Ok(Self {
            plist,
            objects,
            root,
        })
    }

    pub(super) fn root(&self) -> Result<Value<'a>, QlabError> {
        self.resolve(self.root)
    }

    /// `value` with a `Uid` followed to its object; `$null` reads as [`Value::Null`].
    pub(super) fn resolve(&self, value: Value<'a>) -> Result<Value<'a>, QlabError> {
        let Value::Uid(uid) = value else {
            return Ok(value);
        };
        let index = usize::try_from(uid).map_err(|_| QlabError::Damaged("reference"))?;
        let at = self
            .objects
            .get(index)
            .ok_or(QlabError::Damaged("reference"))?;
        let object = self.plist.object(at)?;
        if is_ascii(object, "$null") {
            return Ok(Value::Null);
        }
        // An archive never refers to a reference; one that does is damaged, and following it
        // could loop.
        if matches!(object, Value::Uid(_)) {
            return Err(QlabError::Damaged("reference"));
        }
        Ok(object)
    }

    /// Charges `bytes` kept by the converted workspace to the import's budget.
    pub(super) fn keep(&self, bytes: usize) -> Result<(), QlabError> {
        self.plist.keep(bytes)
    }

    /// Charges a list of `len` members, refusing one longer than `max`.
    fn listed(&self, len: usize, max: usize) -> Result<(), QlabError> {
        if len > max {
            return Err(QlabError::TooLarge("collection"));
        }
        self.plist.charge(len)
    }

    /// The fields of the archived object `value` (resolved first); `None` if it is not one.
    pub(super) fn fields(&self, value: Value<'a>) -> Result<Option<Fields<'a>>, QlabError> {
        let Value::Dict { keys, values } = self.resolve(value)? else {
            return Ok(None);
        };
        self.listed(keys.len(), MAX_FIELDS)?;
        let mut out = Vec::with_capacity(keys.len());
        for (k, v) in keys.iter().zip(values.iter()) {
            if let Value::Ascii(key) = self.plist.object(k)? {
                out.push((key, self.plist.object(v)?));
            }
        }
        Ok(Some(Fields(out)))
    }

    /// The class name of an archived object.
    pub(super) fn class(&self, fields: &Fields<'a>) -> Result<Option<String>, QlabError> {
        let Some(class) = fields.get("$class") else {
            return Ok(None);
        };
        let Some(class) = self.fields(class)? else {
            return Ok(None);
        };
        Ok(class.get("$classname").and_then(text))
    }

    /// A string: a plain string object, an `NSString`, or the text of an `NSAttributedString`.
    pub(super) fn string(&self, value: Option<Value<'a>>) -> Result<Option<String>, QlabError> {
        let Some(value) = value else {
            return Ok(None);
        };
        let value = self.resolve(value)?;
        if let Some(s) = text(value) {
            return Ok(Some(s));
        }
        let Some(fields) = self.fields(value)? else {
            return Ok(None);
        };
        if let Some(s) = fields.get("NS.string") {
            return Ok(text(self.resolve(s)?));
        }
        // An attributed string keeps its characters in `NSString`, itself a string object.
        match fields.get("NSString") {
            Some(s) => {
                let inner = self.resolve(s)?;
                if let Some(t) = text(inner) {
                    return Ok(Some(t));
                }
                Ok(self
                    .fields(inner)?
                    .and_then(|f| f.get("NS.string"))
                    .map(|s| self.resolve(s))
                    .transpose()?
                    .and_then(text))
            }
            None => Ok(None),
        }
    }

    /// A number (an integer, real or boolean, stored inline or as an object); `None` for
    /// anything else and for a number that is not finite.
    pub(super) fn number(&self, value: Option<Value<'a>>) -> Result<Option<f64>, QlabError> {
        let Some(value) = value else {
            return Ok(None);
        };
        Ok(match self.resolve(value)? {
            Value::Int(n) => Some(n as f64),
            Value::Real(r) if r.is_finite() => Some(r),
            Value::Bool(b) => Some(if b { 1.0 } else { 0.0 }),
            _ => None,
        })
    }

    /// A boolean: `true`/`false`, or a number (non-zero is true).
    pub(super) fn boolean(&self, value: Option<Value<'a>>) -> Result<Option<bool>, QlabError> {
        Ok(self.number(value)?.map(|n| n != 0.0))
    }

    /// The members of an `NSArray` or `NSSet` (unresolved).
    pub(super) fn array(&self, value: Option<Value<'a>>) -> Result<Vec<Value<'a>>, QlabError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let Some(fields) = self.fields(value)? else {
            return Ok(Vec::new());
        };
        let Some(Value::Array(refs)) = fields.get("NS.objects") else {
            return Ok(Vec::new());
        };
        self.listed(refs.len(), MAX_ITEMS)?;
        refs.iter().map(|r| self.plist.object(r)).collect()
    }

    /// The entries of an `NSDictionary` with string keys (values unresolved).
    pub(super) fn dict(
        &self,
        value: Option<Value<'a>>,
    ) -> Result<Vec<(String, Value<'a>)>, QlabError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let Some(fields) = self.fields(value)? else {
            return Ok(Vec::new());
        };
        let (Some(Value::Array(keys)), Some(Value::Array(values))) =
            (fields.get("NS.keys"), fields.get("NS.objects"))
        else {
            return Ok(Vec::new());
        };
        let len = keys.len().min(values.len());
        self.listed(len, MAX_ITEMS)?;
        let mut out = Vec::with_capacity(len);
        for (k, v) in keys.iter().zip(values.iter()) {
            let k = self.resolve(self.plist.object(k)?)?;
            // Keys are strings, or numbers (a level matrix's cells).
            let key = match k {
                Value::Int(n) => Some(n.to_string()),
                Value::Ascii(b) | Value::Utf16(b) if b.len() > MAX_KEY_BYTES => None,
                k => self.string(Some(k))?.filter(|s| s.len() <= MAX_KEY_BYTES),
            };
            if let Some(key) = key {
                out.push((key, self.plist.object(v)?));
            }
        }
        Ok(out)
    }

    /// The value under `key` of an `NSDictionary`.
    pub(super) fn entry(
        &self,
        value: Option<Value<'a>>,
        key: &str,
    ) -> Result<Option<Value<'a>>, QlabError> {
        Ok(self
            .dict(value)?
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v))
    }

    /// The bytes of an `NSData` (or a plain data object).
    pub(super) fn data(&self, value: Option<Value<'a>>) -> Result<Option<&'a [u8]>, QlabError> {
        let Some(value) = value else {
            return Ok(None);
        };
        let value = self.resolve(value)?;
        if let Value::Data(bytes) = value {
            return Ok(Some(bytes));
        }
        Ok(match self.fields(value)?.and_then(|f| f.get("NS.data")) {
            Some(inner) => match self.resolve(inner)? {
                Value::Data(bytes) => Some(bytes),
                _ => None,
            },
            None => None,
        })
    }
}
