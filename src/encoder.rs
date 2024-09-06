// Copyright 2020-2024 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use super::Disclosure;
use super::Hasher;
#[cfg(feature = "sha")]
use super::Sha256Hasher;
use crate::Error;
use crate::Result;
use json_pointer::JsonPointer;
use rand::Rng;
use serde::Serialize;
use serde_json::json;
use serde_json::Map;
use serde_json::Value;

pub(crate) const DIGESTS_KEY: &str = "_sd";
pub(crate) const ARRAY_DIGEST_KEY: &str = "...";
pub(crate) const DEFAULT_SALT_SIZE: usize = 30;
pub(crate) const SD_ALG: &str = "_sd_alg";
pub const HEADER_TYP: &str = "sd-jwt";

/// Transforms a JSON object into an SD-JWT object by substituting selected values
/// with their corresponding disclosure digests.
#[derive(Debug, Clone)]
pub struct SdObjectEncoder<H> {
  /// The object in JSON format.
  pub(crate) object: Value,
  /// Size of random data used to generate the salts for disclosures in bytes.
  /// Constant length for readability considerations.
  pub(crate) salt_size: usize,
  /// The hash function used to create digests.
  pub(crate) hasher: H,
}

#[cfg(feature = "sha")]
impl SdObjectEncoder<Sha256Hasher> {
  /// Creates a new [`SdObjectEncoder`] with `sha-256` hash function.
  ///
  /// ## Error
  /// Returns [`Error::DataTypeMismatch`] if `object` is not a valid JSON object.
  pub fn new(object: Value) -> Result<Self> {
    Self::try_from(object)
  }

  /// Creates a new [`SdObjectEncoder`] with `sha-256` hash function from a serializable object.
  ///
  /// ## Error
  /// Returns [`Error::DeserializationError`] if `object` can not be serialized into a valid JSON object.
  pub fn try_from_serializable<T>(object: T) -> Result<Self>
  where
    T: Serialize,
  {
    let object = serde_json::to_value(&object).map_err(|e| Error::DeserializationError(e.to_string()))?;
    SdObjectEncoder::try_from(object)
  }
}

#[cfg(feature = "sha")]
impl TryFrom<Value> for SdObjectEncoder<Sha256Hasher> {
  type Error = crate::Error;
  fn try_from(value: Value) -> Result<Self> {
    Self::with_custom_hasher(value, Sha256Hasher::new())
  }
}

impl<H: Hasher> SdObjectEncoder<H> {
  /// Creates a new [`SdObjectEncoder`] with custom hash function to create digests.
  pub fn with_custom_hasher(object: Value, hasher: H) -> Result<Self> {
    Self::with_custom_hasher_and_salt_size(object, hasher, DEFAULT_SALT_SIZE)
  }
  /// Creates a new [`SdObjectEncoder`] with custom hash function to create digests, and custom salt size.
  pub fn with_custom_hasher_and_salt_size(object: Value, hasher: H, salt_size: usize) -> Result<Self> {
    if !object.is_object() {
      return Err(Error::DataTypeMismatch(
        "argument `object` must be a JSON Object".to_string(),
      ));
    };

    Ok(Self {
      object,
      salt_size,
      hasher,
    })
  }

  /// Substitutes a value with the digest of its disclosure.
  ///
  /// `path` indicates the pointer to the value that will be concealed using the syntax of
  /// [JSON pointer](https://datatracker.ietf.org/doc/html/rfc6901).
  ///
  ///
  /// ## Example
  ///  ```
  ///  use sd_jwt_payload::SdObjectEncoder;
  ///  use sd_jwt_payload::json;
  ///
  ///  let obj = json!({
  ///   "id": "did:value",
  ///   "claim1": {
  ///      "abc": true
  ///   },
  ///   "claim2": ["val_1", "val_2"]
  /// });
  /// let mut encoder = SdObjectEncoder::try_from(obj).unwrap();
  /// encoder.conceal("/id").unwrap(); //conceals "id": "did:value"
  /// encoder.conceal("/claim1/abc").unwrap(); //"abc": true
  /// encoder.conceal("/claim2/0").unwrap(); //conceals "val_1"
  /// ```
  /// 
  /// ## Error
  /// * [`Error::InvalidPath`] if pointer is invalid.
  /// * [`Error::DataTypeMismatch`] if existing SD format is invalid.
  pub fn conceal(&mut self, path: &str) -> Result<Disclosure> {
    // Determine salt.
    let salt = Self::gen_rand(self.salt_size);

    let element_pointer = path
      .parse::<JsonPointer<_, _>>()
      .map_err(|err| Error::InvalidPath(format!("{:?}", err)))?;

    let mut parent_pointer = element_pointer.clone();
    let element_key = parent_pointer
      .pop()
      .ok_or(Error::InvalidPath("path does not contain any values".to_string()))?;

    let parent = parent_pointer
      .get(&self.object)
      .map_err(|err| Error::InvalidPath(format!("{:?}", err)))?;

    match parent {
      Value::Object(_) => {
        let parent = parent_pointer
          .get_mut(&mut self.object)
          .map_err(|err| Error::InvalidPath(format!("{:?}", err)))?
          .as_object_mut()
          .ok_or(Error::InvalidPath("path does not contain any values".to_string()))?;

        // Remove the value from the parent and create a disclosure for it.
        let disclosure = Disclosure::new(
          salt,
          Some(element_key.to_owned()),
          parent
            .remove(&element_key)
            .ok_or(Error::InvalidPath(format!("{} does not exist", element_key)))?,
        );

        // Hash the disclosure.
        let hash = self.hasher.encoded_digest(&disclosure.to_string());

        // Add the hash to the "_sd" array if exists; otherwise, create the array and insert the hash.
        Self::add_digest_to_object(parent, hash)?;
        Ok(disclosure)
      }
      Value::Array(_) => {
        let element = element_pointer.get_mut(&mut self.object).unwrap();
        let disclosure = Disclosure::new(salt, None, element.clone());
        let hash = self.hasher.encoded_digest(&disclosure.to_string());
        let tripledot = json!({ARRAY_DIGEST_KEY: hash});
        *element = tripledot;
        Ok(disclosure)
      }
      _ => Err(crate::Error::Unspecified(
        "parent of element can can only be an object or an array".to_string(),
      )),
    }
  }

  /// Adds the `_sd_alg` property to the top level of the object.
  /// The value is taken from the [`crate::Hasher::alg_name`] implementation.
  pub fn add_sd_alg_property(&mut self) {
    self
      .object
      .as_object_mut()
      // Safety: `object` is a JSON object.
      .unwrap()
      .insert(SD_ALG.to_string(), Value::String(self.hasher.alg_name().to_string()));
  }

  /// Returns the modified object as a string.
  pub fn try_to_string(&self) -> Result<String> {
    serde_json::to_string(&self.object)
      .map_err(|_e| Error::Unspecified("error while serializing internal object".to_string()))
  }

  /// Adds a decoy digest to the specified path.
  ///
  /// `path` indicates the pointer to the value that will be concealed using the syntax of
  /// [JSON pointer](https://datatracker.ietf.org/doc/html/rfc6901).
  ///
  /// Use `path` = "" to add decoys to the top level.
  pub fn add_decoys(&mut self, path: &str, number_of_decoys: usize) -> Result<()> {
    for _ in 0..number_of_decoys {
      self.add_decoy(path)?;
    }
    Ok(())
  }

  fn add_decoy(&mut self, path: &str) -> Result<()> {
    let mut element_pointer = path
      .parse::<JsonPointer<_, _>>()
      .map_err(|err| Error::InvalidPath(format!("{:?}", err)))?;

    let value = element_pointer
      .get_mut(&mut self.object)
      .map_err(|err| Error::InvalidPath(format!("{:?}", err)))?;
    if let Some(object) = value.as_object_mut() {
      let (_, hash) = Self::random_digest(&self.hasher, self.salt_size, false);
      Self::add_digest_to_object(object, hash)?;
      Ok(())
    } else if let Some(array) = value.as_array_mut() {
      let (_, hash) = Self::random_digest(&self.hasher, self.salt_size, true);
      let tripledot = json!({ARRAY_DIGEST_KEY: hash});
      array.push(tripledot);
      Ok(())
    } else {
      Err(Error::InvalidPath(format!(
        "{:?} is neither an object nor an array",
        element_pointer.pop()
      )))
    }
  }

  /// Add the hash to the "_sd" array if exists; otherwise, create the array and insert the hash.
  fn add_digest_to_object(object: &mut Map<String, Value>, digest: String) -> Result<()> {
    if let Some(sd_value) = object.get_mut(DIGESTS_KEY) {
      if let Value::Array(value) = sd_value {
        value.push(Value::String(digest))
      } else {
        return Err(Error::DataTypeMismatch(
          "invalid object: existing `_sd` type is not an array".to_string(),
        ));
      }
    } else {
      object.insert(DIGESTS_KEY.to_owned(), Value::Array(vec![Value::String(digest)]));
    }
    Ok(())
  }

  fn random_digest(hasher: &dyn Hasher, salt_len: usize, array_entry: bool) -> (Disclosure, String) {
    let mut rng = rand::thread_rng();
    let salt = Self::gen_rand(salt_len);
    let decoy_value_length = rng.gen_range(20..=100);
    let decoy_claim_name = if array_entry {
      None
    } else {
      let decoy_claim_name_length = rng.gen_range(4..=10);
      Some(Self::gen_rand(decoy_claim_name_length))
    };
    let decoy_value = Self::gen_rand(decoy_value_length);
    let disclosure = Disclosure::new(salt, decoy_claim_name, Value::String(decoy_value));
    let hash = hasher.encoded_digest(&disclosure.to_string());
    (disclosure, hash)
  }

  fn gen_rand(len: usize) -> String {
    let mut bytes = vec![0; len];
    let mut rng = rand::thread_rng();
    rng.fill(&mut bytes[..]);

    multibase::Base::Base64Url.encode(bytes)
  }

  /// Returns a reference to the internal object.
  pub fn object(&self) -> &Map<String, Value> {
    // Safety: encoder can be constructed from objects only.
    self.object.as_object().unwrap()
  }

  /// Returns the used salt length.
  pub fn salt_size(&self) -> usize {
    self.salt_size
  }
}

#[cfg(test)]
mod test {

  use super::SdObjectEncoder;
  use crate::Error;
  use serde::Serialize;
  use serde_json::json;
  use serde_json::Value;

  #[derive(Serialize)]
  struct TestStruct {
    id: String,
    claim2: Vec<String>,
  }

  fn object() -> Value {
    json!({
      "id": "did:value",
      "claim1": {
        "abc": true
      },
      "claim2": ["arr-value1", "arr-value2"]
    })
  }

  #[test]
  fn simple() {
    let mut encoder = SdObjectEncoder::try_from(object()).unwrap();
    encoder.conceal("/claim1/abc").unwrap();
    encoder.conceal("/id").unwrap();
    encoder.add_decoys("", 10).unwrap();
    encoder.add_decoys("/claim2", 10).unwrap();
    assert!(encoder.object().get("id").is_none());
    assert_eq!(encoder.object.get("_sd").unwrap().as_array().unwrap().len(), 11);
    assert_eq!(encoder.object.get("claim2").unwrap().as_array().unwrap().len(), 12);
  }

  #[test]
  fn errors() {
    let mut encoder = SdObjectEncoder::try_from(object()).unwrap();
    encoder.conceal("/claim1/abc").unwrap();
    assert!(matches!(
      encoder.conceal("claim2/2").unwrap_err(),
      Error::InvalidPath(_)
    ));
  }

  #[test]
  fn test_wrong_path() {
    let mut encoder = SdObjectEncoder::try_from(object()).unwrap();
    assert!(matches!(
      encoder.conceal("/claim12").unwrap_err(),
      Error::InvalidPath(_)
    ));
    assert!(matches!(
      encoder.conceal("/claim12/0").unwrap_err(),
      Error::InvalidPath(_)
    ));
  }

  #[test]
  fn test_from_serializable() {
    let test_value = TestStruct {
      id: "did:value".to_string(),
      claim2: vec!["arr-value1".to_string(), "arr-vlaue2".to_string()],
    };
    let mut encoder = SdObjectEncoder::try_from_serializable(test_value).unwrap();
    encoder.conceal("/id").unwrap();
    encoder.add_decoys("", 10).unwrap();
    encoder.add_decoys("/claim2", 10).unwrap();
    assert!(encoder.object.get("id").is_none());
    assert_eq!(encoder.object.get("_sd").unwrap().as_array().unwrap().len(), 11);
    assert_eq!(encoder.object.get("claim2").unwrap().as_array().unwrap().len(), 12);
  }
}
