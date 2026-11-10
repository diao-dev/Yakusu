//! Unity SerializedFiles (the "CAB-..." files inside bundles, and
//! `.assets` files): the object table, and TextAsset objects.
//!
//! Only what's needed to find objects and change their size is read: the
//! type list is walked over, the object table is kept with the byte
//! positions of each object's offset and size fields, so objects can be
//! replaced and the table patched in place. Formats 9 to 23 (Unity 5 to 6).

use crate::error::{Error, Result};

pub const TEXT_ASSET: i32 = 49;

#[derive(Clone, Debug)]
pub struct Object {
    pub path_id: i64,
    pub class_id: i32,
    /// Absolute position and size of the object's data in the file.
    pub start: u64,
    pub size: u32,
    /// Index into the file's types (for the type tree).
    pub type_index: Option<usize>,
    start_field: usize,
    size_field: usize,
}

/// A type tree node (field), with its children.
#[derive(Clone, Debug)]
pub struct TypeNode {
    pub ty: String,
    pub name: String,
    pub meta: i32,
    pub children: Vec<TypeNode>,
}

/// Where a string sits in an object: its length field, and its field path.
#[derive(Clone, Debug)]
pub struct StrField {
    pub at: usize,
    pub len: usize,
    pub path: String,
    pub text: String,
}

pub struct SerializedFile {
    pub version: u32,
    pub unity: String,
    little: bool,
    data_offset: u64,
    pub objects: Vec<Object>,
    /// Per type: class id and its type tree (when the file carries them).
    pub types: Vec<(i32, Option<TypeNode>)>,
}

fn bad(why: impl std::fmt::Display) -> Error {
    Error::User(format!("A Unity data file couldn't be read ({why})."))
}

struct R<'a> {
    b: &'a [u8],
    i: usize,
    little: bool,
}

impl R<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.i.checked_add(n).ok_or_else(|| bad("cut short"))?;
        let s = self.b.get(self.i..end).ok_or_else(|| bad("cut short"))?;
        self.i = end;
        Ok(s)
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b: [u8; 2] = self.take(2)?.try_into().unwrap();
        Ok(if self.little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }
    fn u32(&mut self) -> Result<u32> {
        let b: [u8; 4] = self.take(4)?.try_into().unwrap();
        Ok(if self.little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn u64(&mut self) -> Result<u64> {
        let b: [u8; 8] = self.take(8)?.try_into().unwrap();
        Ok(if self.little { u64::from_le_bytes(b) } else { u64::from_be_bytes(b) })
    }
    fn cstr(&mut self) -> Result<String> {
        let end = self.b[self.i..].iter().position(|&c| c == 0).ok_or_else(|| bad("cut short"))?;
        let s = String::from_utf8_lossy(&self.b[self.i..self.i + end]).to_string();
        self.i += end + 1;
        Ok(s)
    }
    fn align(&mut self, n: usize) {
        self.i = self.i.div_ceil(n) * n;
    }
    fn count(&mut self, max: usize) -> Result<usize> {
        let n = self.i32()?;
        if n < 0 || n as usize > max {
            return Err(bad("damaged table"));
        }
        Ok(n as usize)
    }
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

/// Whether bytes look like a SerializedFile (not a .resS resource).
pub fn looks_serialized(b: &[u8]) -> bool {
    if b.len() < 48 {
        return false;
    }
    let v = be32(b, 8);
    (9..=30).contains(&v) && (b[16] == 0 || b[16] == 1) && {
        let (file_size, data_offset) =
            if v >= 22 { (be64(b, 24), be64(b, 32)) } else { (be32(b, 4) as u64, be32(b, 12) as u64) };
        file_size == b.len() as u64 && data_offset <= file_size
    }
}

impl SerializedFile {
    pub fn parse(b: &[u8]) -> Result<SerializedFile> {
        Self::parse_head(b, b.len() as u64)
    }

    /// Parse from the first bytes of a file (at least up to its data
    /// offset); `total` is the whole file's length.
    pub fn parse_head(b: &[u8], total: u64) -> Result<SerializedFile> {
        if b.len() < 20 {
            return Err(bad("too short"));
        }
        let version = be32(b, 8);
        if !(9..=30).contains(&version) {
            return Err(bad(format!("format {version} isn't supported")));
        }
        let little = b[16] == 0;
        let (data_offset, meta_at) = if version >= 22 { (be64(b, 32), 48) } else { (be32(b, 12) as u64, 20) };
        let mut r = R { b, i: meta_at, little };
        let unity = r.cstr()?;
        r.skip(4)?; // target platform
        let type_tree = if version >= 13 { r.u8()? != 0 } else { true };
        let n_types = r.count(1 << 16)?;
        let mut types = Vec::with_capacity(n_types);
        for _ in 0..n_types {
            types.push(read_type(&mut r, version, type_tree, false)?);
        }
        let big_ids = if (7..14).contains(&version) { r.i32()? != 0 } else { false };
        let n_obj = r.count(1 << 24)?;
        let mut objects = Vec::with_capacity(n_obj);
        for _ in 0..n_obj {
            let path_id = if big_ids {
                r.u64()? as i64
            } else if version < 14 {
                r.i32()? as i64
            } else {
                r.align(4);
                r.u64()? as i64
            };
            let start_field = r.i;
            let start = if version >= 22 { r.u64()? } else { r.u32()? as u64 } + data_offset;
            let size_field = r.i;
            let size = r.u32()?;
            let type_id = r.i32()?;
            let (class_id, type_index) = if version < 16 {
                let c = r.u16()? as i32;
                (c, types.iter().position(|(id, _)| *id == type_id))
            } else {
                let t = types.get(type_id as usize).ok_or_else(|| bad("unknown object type"))?;
                (t.0, Some(type_id as usize))
            };
            if version < 11 {
                r.skip(2)?;
            }
            if (11..17).contains(&version) {
                r.i16()?;
            }
            if version == 15 || version == 16 {
                r.skip(1)?;
            }
            if start + size as u64 > total {
                return Err(bad("an object runs past the end"));
            }
            objects.push(Object { path_id, class_id, start, size, type_index, start_field, size_field });
        }
        Ok(SerializedFile { version, unity, little, data_offset, objects, types })
    }

    /// The file with some objects' data replaced (path id -> new bytes).
    /// Objects keep their order; each starts 8-byte aligned, as Unity writes.
    pub fn rebuild(&self, b: &[u8], replace: &std::collections::HashMap<i64, Vec<u8>>) -> Vec<u8> {
        let mut out = b[..self.data_offset as usize].to_vec();
        let mut order: Vec<&Object> = self.objects.iter().collect();
        order.sort_by_key(|o| o.start);
        let mut placed: Vec<(&Object, u64, u32)> = Vec::new();
        let mut last_end = self.data_offset as usize;
        for o in order {
            // Zero padding between objects becomes 8-byte alignment; anything
            // else between them is kept as it was.
            let gap = &b[last_end.min(o.start as usize)..o.start as usize];
            if gap.iter().all(|&x| x == 0) {
                while !out.len().is_multiple_of(8) {
                    out.push(0);
                }
            } else {
                out.extend_from_slice(gap);
            }
            let bytes: &[u8] = match replace.get(&o.path_id) {
                Some(n) => n,
                None => self.object(b, o),
            };
            placed.push((o, out.len() as u64 - self.data_offset, bytes.len() as u32));
            out.extend_from_slice(bytes);
            last_end = last_end.max(o.start as usize + o.size as usize);
        }
        if last_end < b.len() {
            out.extend_from_slice(&b[last_end..]);
        }
        for (o, start, size) in placed {
            if self.version >= 22 {
                put(&mut out, o.start_field, &start.to_le_bytes(), &start.to_be_bytes(), self.little);
            } else {
                let s = start as u32;
                put(&mut out, o.start_field, &s.to_le_bytes(), &s.to_be_bytes(), self.little);
            }
            put(&mut out, o.size_field, &size.to_le_bytes(), &size.to_be_bytes(), self.little);
        }
        // Header: total size (always big-endian).
        let len = out.len() as u64;
        if self.version >= 22 {
            out[24..32].copy_from_slice(&len.to_be_bytes());
        } else {
            out[4..8].copy_from_slice(&(len as u32).to_be_bytes());
        }
        out
    }

    pub fn object<'a>(&self, b: &'a [u8], o: &Object) -> &'a [u8] {
        &b[o.start as usize..o.start as usize + o.size as usize]
    }

    /// A TextAsset's name and text bytes, and where the rest of it begins.
    pub fn text_asset(&self, obj: &[u8]) -> Option<(String, Vec<u8>, usize)> {
        let mut r = R { b: obj, i: 0, little: self.little };
        let n = r.u32().ok()? as usize;
        let name = String::from_utf8_lossy(r.take(n).ok()?).to_string();
        r.align(4);
        let n = r.u32().ok()? as usize;
        let script = r.take(n).ok()?.to_vec();
        r.align(4);
        (r.i <= obj.len()).then_some((name, script, r.i))
    }

    /// A TextAsset object with new text (everything after the text kept).
    pub fn with_text(&self, obj: &[u8], text: &[u8]) -> Option<Vec<u8>> {
        let (name, _, rest) = self.text_asset(obj)?;
        let mut out = Vec::with_capacity(obj.len() + text.len());
        let n = |v: u32| if self.little { v.to_le_bytes() } else { v.to_be_bytes() };
        out.extend(n(name.len() as u32));
        out.extend(name.as_bytes());
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out.extend(n(text.len() as u32));
        out.extend(text);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out.extend_from_slice(&obj[rest..]);
        Some(out)
    }
}

fn put(out: &mut [u8], at: usize, le: &[u8], be: &[u8], little: bool) {
    out[at..at + le.len()].copy_from_slice(if little { le } else { be });
}

/// Unity's shared type tree strings (offsets with the top bit set).
const COMMON: &[&str] = &[
    "AABB",
    "AnimationClip",
    "AnimationCurve",
    "AnimationState",
    "Array",
    "Base",
    "BitField",
    "bitset",
    "bool",
    "char",
    "ColorRGBA",
    "Component",
    "data",
    "deque",
    "double",
    "dynamic_array",
    "FastPropertyName",
    "first",
    "float",
    "Font",
    "GameObject",
    "Generic Mono",
    "GradientNEW",
    "GUID",
    "GUIStyle",
    "int",
    "list",
    "long long",
    "map",
    "Matrix4x4f",
    "MdFour",
    "MonoBehaviour",
    "MonoScript",
    "m_ByteSize",
    "m_Curve",
    "m_EditorClassIdentifier",
    "m_EditorHideFlags",
    "m_Enabled",
    "m_ExtensionPtr",
    "m_GameObject",
    "m_Index",
    "m_IsArray",
    "m_IsStatic",
    "m_MetaFlag",
    "m_Name",
    "m_ObjectHideFlags",
    "m_PrefabInternal",
    "m_PrefabParentObject",
    "m_Script",
    "m_StaticEditorFlags",
    "m_Type",
    "m_Version",
    "Object",
    "pair",
    "PPtr<Component>",
    "PPtr<GameObject>",
    "PPtr<Material>",
    "PPtr<MonoBehaviour>",
    "PPtr<MonoScript>",
    "PPtr<Object>",
    "PPtr<Prefab>",
    "PPtr<Sprite>",
    "PPtr<TextAsset>",
    "PPtr<Texture>",
    "PPtr<Texture2D>",
    "PPtr<Transform>",
    "Prefab",
    "Quaternionf",
    "Rectf",
    "RectInt",
    "RectOffset",
    "second",
    "set",
    "short",
    "size",
    "SInt16",
    "SInt32",
    "SInt64",
    "SInt8",
    "staticvector",
    "string",
    "TextAsset",
    "TextMesh",
    "Texture",
    "Texture2D",
    "Transform",
    "TypelessData",
    "UInt16",
    "UInt32",
    "UInt64",
    "UInt8",
    "unsigned int",
    "unsigned long long",
    "unsigned short",
    "vector",
    "Vector2f",
    "Vector3f",
    "Vector4f",
    "m_ScriptingClassIdentifier",
    "Gradient",
    "Type*",
    "int2_storage",
    "int3_storage",
    "BoundsInt",
    "m_CorrespondingSourceObject",
    "m_PrefabInstance",
    "m_PrefabAsset",
    "FileSize",
    "Hash128",
    "RenderingLayerMask",
];

fn common(offset: u32) -> String {
    let mut at = 0u32;
    for s in COMMON {
        if at == offset {
            return s.to_string();
        }
        at += s.len() as u32 + 1;
    }
    format!("?{offset}")
}

/// A type tree in blob form, as a tree.
fn read_tree(r: &mut R, version: u32) -> Result<TypeNode> {
    let n = r.count(1 << 20)?;
    let strings_len = r.count(1 << 26)?;
    let mut flat = Vec::with_capacity(n);
    for _ in 0..n {
        r.skip(2)?; // version
        let level = r.u8()?;
        r.skip(1)?; // type flags
        let ty = r.u32()?;
        let name = r.u32()?;
        r.skip(8)?; // byte size, index
        let meta = r.i32()?;
        if version >= 19 {
            r.skip(8)?;
        }
        flat.push((level, ty, name, meta));
    }
    let buf = r.take(strings_len)?.to_vec();
    let s = |v: u32| -> String {
        if v & 0x8000_0000 != 0 {
            common(v & 0x7FFF_FFFF)
        } else {
            let at = (v as usize).min(buf.len());
            let end = buf[at..].iter().position(|&c| c == 0).map(|e| at + e).unwrap_or(buf.len());
            String::from_utf8_lossy(&buf[at..end]).to_string()
        }
    };
    // Build the tree from levels.
    let mut stack: Vec<(u8, TypeNode)> = Vec::new();
    let mut root: Option<TypeNode> = None;
    for (level, ty, name, meta) in flat {
        let node = TypeNode { ty: s(ty), name: s(name), meta, children: vec![] };
        while stack.last().is_some_and(|(l, _)| *l >= level) {
            let (_, done) = stack.pop().unwrap();
            match stack.last_mut() {
                Some((_, parent)) => parent.children.push(done),
                None => root = Some(done),
            }
        }
        stack.push((level, node));
    }
    while let Some((_, done)) = stack.pop() {
        match stack.last_mut() {
            Some((_, parent)) => parent.children.push(done),
            None => root = Some(done),
        }
    }
    root.ok_or_else(|| bad("empty type tree"))
}

/// Read one SerializedType: its class id and type tree.
fn read_type(r: &mut R, version: u32, type_tree: bool, is_ref: bool) -> Result<(i32, Option<TypeNode>)> {
    let class_id = r.i32()?;
    if version >= 16 {
        r.skip(1)?;
    }
    let script_index = if version >= 17 { r.i16()? } else { -1 };
    if version >= 13 {
        if (is_ref && script_index >= 0) || (version < 16 && class_id < 0) || (version >= 16 && class_id == 114) {
            r.skip(16)?;
        }
        r.skip(16)?;
    }
    let mut tree = None;
    if type_tree {
        if version >= 23 {
            r.skip(16)?;
            let n = r.u32()? as usize;
            if n >= 8 {
                r.skip(n)?;
            }
        } else if version >= 12 || version == 10 {
            tree = Some(read_tree(r, version)?);
        } else {
            return Err(bad(format!("format {version} type trees aren't supported")));
        }
        if version >= 21 {
            if is_ref {
                r.cstr()?;
                r.cstr()?;
                r.cstr()?;
            } else {
                let n = r.count(1 << 20)?;
                r.skip(n * 4)?;
            }
        }
    }
    Ok((class_id, tree))
}

// ---------------------------------------------------------------- walking objects

fn prim_size(ty: &str) -> Option<usize> {
    Some(match ty {
        "SInt8" | "UInt8" | "char" | "bool" => 1,
        "SInt16" | "UInt16" | "short" | "unsigned short" => 2,
        "SInt32" | "UInt32" | "int" | "unsigned int" | "float" | "Type*" => 4,
        "SInt64" | "UInt64" | "long long" | "unsigned long long" | "double" | "FileSize" => 8,
        _ => return None,
    })
}

/// Stops the walk where Yakusu can't follow the layout (managed references).
struct Stop;

fn walk(node: &TypeNode, r: &mut R, path: &str, out: &mut Vec<StrField>) -> std::result::Result<(), Stop> {
    let mut align = node.meta & 0x4000 != 0;
    let here = if path.is_empty() { node.name.clone() } else { format!("{path}.{}", node.name) };
    if let Some(n) = prim_size(&node.ty) {
        r.skip(n).map_err(|_| Stop)?;
    } else if node.ty == "string" {
        let at = r.i;
        let len = r.u32().map_err(|_| Stop)? as usize;
        let bytes = r.take(len).map_err(|_| Stop)?;
        out.push(StrField { at, len, path: here, text: String::from_utf8_lossy(bytes).to_string() });
        align = true;
    } else if node.ty == "TypelessData" {
        let n = r.u32().map_err(|_| Stop)? as usize;
        r.skip(n).map_err(|_| Stop)?;
    } else if node.ty == "ManagedReferencesRegistry" || node.ty == "ReferencedObject" {
        return Err(Stop);
    } else if let Some(arr) = node.children.first().filter(|c| c.ty == "Array" || c.name == "Array") {
        if arr.meta & 0x4000 != 0 {
            align = true;
        }
        let n = r.u32().map_err(|_| Stop)? as usize;
        let elem = arr.children.get(1).ok_or(Stop)?;
        if let Some(sz) = prim_size(&elem.ty).filter(|_| elem.meta & 0x4000 == 0) {
            r.skip(n.checked_mul(sz).ok_or(Stop)?).map_err(|_| Stop)?;
        } else {
            if n > r.b.len() {
                return Err(Stop);
            }
            let item = format!("{here}[]");
            for _ in 0..n {
                walk(elem, r, &item, out)?;
            }
        }
    } else {
        for c in &node.children {
            walk(c, r, &here, out)?;
        }
    }
    if align {
        r.align(4);
    }
    Ok(())
}

impl SerializedFile {
    /// The strings of an object, walked with its type tree. The walk stops
    /// at managed references (the strings before them are still listed).
    pub fn strings(&self, obj: &[u8], o: &Object) -> Option<Vec<StrField>> {
        let tree = self.types.get(o.type_index?)?.1.as_ref()?;
        let mut r = R { b: obj, i: 0, little: self.little };
        let mut out = Vec::new();
        let _ = walk(tree, &mut r, "", &mut out);
        Some(out)
    }

    /// How many bytes the type tree accounts for (None if the walk stopped).
    pub fn walked_len(&self, obj: &[u8], o: &Object) -> Option<usize> {
        let tree = self.types.get(o.type_index?)?.1.as_ref()?;
        let mut r = R { b: obj, i: 0, little: self.little };
        walk(tree, &mut r, "", &mut Vec::new()).ok()?;
        Some(r.i)
    }

    /// An object with some of its strings replaced (by `at`).
    pub fn with_strings(&self, obj: &[u8], new: &[(StrField, String)]) -> Vec<u8> {
        let mut sorted: Vec<&(StrField, String)> = new.iter().collect();
        sorted.sort_by_key(|(f, _)| f.at);
        let mut out = Vec::with_capacity(obj.len() + 1024);
        let mut pos = 0;
        for (f, text) in sorted {
            out.extend_from_slice(&obj[pos..f.at]);
            let b = text.as_bytes();
            let n = b.len() as u32;
            out.extend(if self.little { n.to_le_bytes() } else { n.to_be_bytes() });
            out.extend_from_slice(b);
            // The padding after a string depends on where it ends.
            let old_end = f.at + 4 + f.len;
            let old_pad = old_end.div_ceil(4) * 4 - old_end;
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
            pos = (old_end + old_pad).min(obj.len());
        }
        out.extend_from_slice(&obj[pos..]);
        out
    }
}
