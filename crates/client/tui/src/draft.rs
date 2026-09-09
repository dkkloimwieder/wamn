//! Editable request fields with explicit presence and protected bindings.

use std::fmt;

use serde_json::{Map, Value};
use wamn_client::descriptor::FieldSchema;
use wamn_client::request::{BuiltRequest, RequestError, build_request};

/// A property's presence, independently of whether its value is null.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldState {
    Absent,
    Null,
    Value(Value),
}

/// The control supported by a declared field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// Text entered under the field's declared scalar type.
    Text,
    Choice,
    Object,
    Repeated,
    Unsupported,
}

/// Select a control without falling back to text for an unknown type.
#[must_use]
pub fn input_kind(schema: &FieldSchema) -> InputKind {
    match schema.field.type_name {
        "object" if !schema.children.is_empty() => InputKind::Object,
        "array" if !schema.children.is_empty() => InputKind::Repeated,
        "text" | "string" | "uuid" | "timestamptz" | "numeric" | "int32" | "int64" | "float64"
        | "boolean" => {
            if schema.field.values.is_empty() {
                InputKind::Text
            } else {
                InputKind::Choice
            }
        }
        _ => InputKind::Unsupported,
    }
}

/// Why an editor operation could not be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftErrorKind {
    InvalidPointer,
    UnknownField,
    UnsupportedField,
    Protected,
    InvalidShape,
    RowBounds,
    RowIndex,
}

/// An editor refusal with the affected JSON Pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftError {
    kind: DraftErrorKind,
    path: String,
}

impl DraftError {
    #[must_use]
    pub const fn kind(&self) -> DraftErrorKind {
        self.kind
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Display for DraftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.kind {
            DraftErrorKind::InvalidPointer => "expected a JSON Pointer to a field",
            DraftErrorKind::UnknownField => "field is not declared",
            DraftErrorKind::UnsupportedField => "field requires a composed editor",
            DraftErrorKind::Protected => "field is supplied by a binding",
            DraftErrorKind::InvalidShape => "value does not support this editor operation",
            DraftErrorKind::RowBounds => "row count would exceed the declared bounds",
            DraftErrorKind::RowIndex => "row index is outside the array",
        };
        write!(formatter, "{}: {reason}", self.path)
    }
}

impl std::error::Error for DraftError {}

fn refusal(kind: DraftErrorKind, path: &str) -> DraftError {
    DraftError {
        kind,
        path: path.to_owned(),
    }
}

/// A single request object edited against its generated field tree.
#[derive(Debug, Clone)]
pub struct Draft {
    fields: &'static [FieldSchema],
    item: Value,
    protected: Vec<Vec<String>>,
}

impl Draft {
    /// Start with every property absent.
    #[must_use]
    pub fn new(fields: &'static [FieldSchema]) -> Self {
        Self {
            fields,
            item: Value::Object(Map::new()),
            protected: Vec::new(),
        }
    }

    /// Inspect the request object without mutating it outside the editor.
    #[must_use]
    pub const fn item(&self) -> &Value {
        &self.item
    }

    /// Read a declared field, including a repeated item's indexed property.
    ///
    /// # Errors
    /// Returns an error for an invalid pointer or an undeclared field.
    pub fn state(&self, pointer: &str) -> Result<FieldState, DraftError> {
        self.resolve(pointer)?;
        Ok(match self.item.pointer(pointer) {
            None => FieldState::Absent,
            Some(Value::Null) => FieldState::Null,
            Some(value) => FieldState::Value(value.clone()),
        })
    }

    /// Reserve an envelope or revision path before presenting the editor.
    ///
    /// # Errors
    /// Returns an error for an invalid pointer or an undeclared field.
    pub fn protect(&mut self, pointer: &str) -> Result<(), DraftError> {
        let (path, _) = self.resolve(pointer)?;
        if !self.protected.contains(&path) {
            self.protected.push(path);
        }
        Ok(())
    }

    /// Supply a trusted binding and protect it from subsequent user edits.
    ///
    /// Missing object parents are created. Array rows must already exist.
    ///
    /// # Errors
    /// Returns an error for an undeclared field or an incompatible parent.
    pub fn bind(&mut self, pointer: &str, value: Value) -> Result<(), DraftError> {
        let (path, _) = self.resolve(pointer)?;
        let mut item = self.item.clone();
        write(
            &mut item,
            self.fields,
            &path,
            FieldState::Value(value),
            pointer,
        )?;
        self.item = item;
        if !self.protected.contains(&path) {
            self.protected.push(path);
        }
        Ok(())
    }

    /// Clear a trusted binding value while retaining its protected input path.
    ///
    /// # Errors
    /// Returns an error for an undeclared field or an incompatible parent.
    pub fn clear_binding(&mut self, pointer: &str) -> Result<(), DraftError> {
        let (path, _) = self.resolve(pointer)?;
        let mut item = self.item.clone();
        write(&mut item, self.fields, &path, FieldState::Absent, pointer)?;
        self.item = item;
        Ok(())
    }

    /// Apply a user edit while preserving absent, null and present values.
    ///
    /// Missing object parents are created for a present value. Use
    /// [`Self::remove_row`] to remove an array item; array items cannot be absent.
    ///
    /// # Errors
    /// Refuses protected or unsupported fields and incompatible parent shapes.
    pub fn edit(&mut self, pointer: &str, state: FieldState) -> Result<(), DraftError> {
        let (path, schema) = self.resolve(pointer)?;
        if schema.kind() == InputKind::Unsupported {
            return Err(refusal(DraftErrorKind::UnsupportedField, pointer));
        }
        if self
            .protected
            .iter()
            .any(|bound| bound.starts_with(&path) || path.starts_with(bound))
        {
            return Err(refusal(DraftErrorKind::Protected, pointer));
        }
        let mut item = self.item.clone();
        write(&mut item, self.fields, &path, state, pointer)?;
        self.item = item;
        Ok(())
    }

    /// Insert a row at an existing position or at the end of a repeated field.
    ///
    /// An absent repeated field starts as an empty array. A new object row can
    /// be `{}` while its required fields are being edited.
    ///
    /// # Errors
    /// Refuses invalid indexes, maximum row bounds and shifts of bound paths.
    pub fn insert_row(
        &mut self,
        pointer: &str,
        index: usize,
        value: Value,
    ) -> Result<(), DraftError> {
        let (path, schema) = self.repeated(pointer, index)?;
        let mut item = self.item.clone();
        if item.pointer(pointer).is_none() {
            write(
                &mut item,
                self.fields,
                &path,
                FieldState::Value(Value::Array(Vec::new())),
                pointer,
            )?;
        }
        let array = item
            .pointer_mut(pointer)
            .and_then(Value::as_array_mut)
            .ok_or_else(|| refusal(DraftErrorKind::InvalidShape, pointer))?;
        if index > array.len() {
            return Err(refusal(DraftErrorKind::RowIndex, pointer));
        }
        let count = u64::try_from(array.len()).expect("array length fits u64");
        if schema.maximum.is_some_and(|maximum| count >= maximum) {
            return Err(refusal(DraftErrorKind::RowBounds, pointer));
        }
        array.insert(index, value);
        self.item = item;
        Ok(())
    }

    /// Remove a row without violating its minimum count or moving a binding.
    ///
    /// # Errors
    /// Refuses invalid indexes, minimum row bounds and shifts of bound paths.
    pub fn remove_row(&mut self, pointer: &str, index: usize) -> Result<(), DraftError> {
        let (_, schema) = self.repeated(pointer, index)?;
        let array = self
            .item
            .pointer_mut(pointer)
            .and_then(Value::as_array_mut)
            .ok_or_else(|| refusal(DraftErrorKind::InvalidShape, pointer))?;
        if index >= array.len() {
            return Err(refusal(DraftErrorKind::RowIndex, pointer));
        }
        let count = u64::try_from(array.len()).expect("array length fits u64");
        if schema.minimum.is_some_and(|minimum| count <= minimum) {
            return Err(refusal(DraftErrorKind::RowBounds, pointer));
        }
        array.remove(index);
        Ok(())
    }

    /// Validate and canonicalize this object into one outer request envelope.
    ///
    /// # Errors
    /// Returns the shared request builder's presence, type or schema refusal.
    pub fn build(&self, input_schema: Option<&Value>) -> Result<BuiltRequest, RequestError> {
        build_request(self.fields, &self.item, input_schema)
    }

    fn resolve(&self, pointer: &str) -> Result<(Vec<String>, Schema), DraftError> {
        let path = pointer_path(pointer)?;
        let schema = schema_at(self.fields, &path)
            .ok_or_else(|| refusal(DraftErrorKind::UnknownField, pointer))?;
        Ok((path, schema))
    }

    fn repeated(
        &self,
        pointer: &str,
        index: usize,
    ) -> Result<(Vec<String>, &'static FieldSchema), DraftError> {
        let (path, Schema::Field(schema)) = self.resolve(pointer)? else {
            return Err(refusal(DraftErrorKind::InvalidShape, pointer));
        };
        if schema.field.type_name != "array" {
            return Err(refusal(DraftErrorKind::InvalidShape, pointer));
        }
        if input_kind(schema) == InputKind::Unsupported {
            return Err(refusal(DraftErrorKind::UnsupportedField, pointer));
        }
        for bound in &self.protected {
            if path.starts_with(bound)
                || (bound.starts_with(&path)
                    && bound
                        .get(path.len())
                        .and_then(|segment| row_index(segment))
                        .is_some_and(|bound_index| index <= bound_index))
            {
                return Err(refusal(DraftErrorKind::Protected, pointer));
            }
        }
        Ok((path, schema))
    }
}

#[derive(Debug, Clone, Copy)]
enum Schema {
    Field(&'static FieldSchema),
    ObjectRow,
}

impl Schema {
    fn kind(self) -> InputKind {
        match self {
            Self::Field(field) => input_kind(field),
            Self::ObjectRow => InputKind::Object,
        }
    }
}

fn row_index(segment: &str) -> Option<usize> {
    if segment == "0" || (!segment.starts_with('0') && segment.bytes().all(|b| b.is_ascii_digit()))
    {
        segment.parse().ok()
    } else {
        None
    }
}

fn pointer_path(pointer: &str) -> Result<Vec<String>, DraftError> {
    let suffix = pointer
        .strip_prefix('/')
        .ok_or_else(|| refusal(DraftErrorKind::InvalidPointer, pointer))?;
    suffix
        .split('/')
        .map(|segment| {
            let mut decoded = String::new();
            let mut chars = segment.chars();
            while let Some(character) = chars.next() {
                decoded.push(if character == '~' {
                    match chars.next() {
                        Some('0') => '~',
                        Some('1') => '/',
                        _ => return Err(refusal(DraftErrorKind::InvalidPointer, pointer)),
                    }
                } else {
                    character
                });
            }
            Ok(decoded)
        })
        .collect()
}

fn schema_at(fields: &'static [FieldSchema], path: &[String]) -> Option<Schema> {
    let (name, mut rest) = path.split_first()?;
    let mut field = fields
        .iter()
        .find(|field| field.field.leaf().trim_end_matches("[]") == name)?;
    loop {
        if rest.is_empty() {
            return Some(Schema::Field(field));
        }
        match field.field.type_name {
            "object" => return schema_at(field.children, rest),
            "array" => {
                if field.children.is_empty() {
                    return None;
                }
                row_index(&rest[0])?;
                rest = &rest[1..];
                if let [item] = field.children
                    && (item.field.path == field.field.path
                        || item.field.path.strip_suffix("[]") == Some(field.field.path))
                {
                    field = item;
                } else if rest.is_empty() {
                    return Some(Schema::ObjectRow);
                } else {
                    return schema_at(field.children, rest);
                }
            }
            _ => return None,
        }
    }
}

fn write(
    item: &mut Value,
    fields: &'static [FieldSchema],
    path: &[String],
    state: FieldState,
    pointer: &str,
) -> Result<(), DraftError> {
    let mut parent = item;
    for (depth, segment) in path[..path.len() - 1].iter().enumerate() {
        parent = match parent {
            Value::Object(object) => {
                if !object.contains_key(segment) {
                    if state == FieldState::Absent {
                        return Ok(());
                    }
                    let initial = match schema_at(fields, &path[..=depth]).map(Schema::kind) {
                        Some(InputKind::Object) => Value::Object(Map::new()),
                        Some(InputKind::Repeated) => Value::Array(Vec::new()),
                        _ => return Err(refusal(DraftErrorKind::InvalidShape, pointer)),
                    };
                    object.insert(segment.clone(), initial);
                }
                object.get_mut(segment).expect("parent was inserted")
            }
            Value::Array(array) => row_index(segment)
                .and_then(|index| array.get_mut(index))
                .ok_or_else(|| refusal(DraftErrorKind::RowIndex, pointer))?,
            _ => return Err(refusal(DraftErrorKind::InvalidShape, pointer)),
        };
    }
    let leaf = path.last().expect("JSON Pointer has a segment");
    match parent {
        Value::Object(object) => match state {
            FieldState::Absent => {
                object.remove(leaf);
            }
            FieldState::Null => {
                object.insert(leaf.clone(), Value::Null);
            }
            FieldState::Value(value) => {
                object.insert(leaf.clone(), value);
            }
        },
        Value::Array(array) => {
            let value = match state {
                FieldState::Absent => return Err(refusal(DraftErrorKind::InvalidShape, pointer)),
                FieldState::Null => Value::Null,
                FieldState::Value(value) => value,
            };
            let slot = row_index(leaf)
                .and_then(|index| array.get_mut(index))
                .ok_or_else(|| refusal(DraftErrorKind::RowIndex, pointer))?;
            *slot = value;
        }
        _ => return Err(refusal(DraftErrorKind::InvalidShape, pointer)),
    }
    Ok(())
}
