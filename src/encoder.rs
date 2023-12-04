// Copyright 2020-2023 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use super::Disclosure;
use super::Hasher;
use super::Sha256Hasher;
use crate::Error;
use crate::Result;
use crate::Utils;
use rand::distributions::DistString;
use rand::Rng;
use serde_json::json;
use serde_json::Map;
use serde_json::Value;

pub(crate) const DIGESTS_KEY: &str = "_sd";
pub(crate) const ARRAY_DIGEST_KEY: &str = "...";
pub(crate) const DEFAULT_SALT_SIZE: usize = 30;

/// Transforms a JSON object into an SD-JWT object by substituting selected values
/// with their corresponding disclosure digests.
pub struct SdObjectEncoder<H: Hasher = Sha256Hasher> {
  /// The object in JSON format.
  object: Map<String, Value>,
  /// Length of the salts that generated for disclosures.
  /// Constant length for readability considerations.
  salt_length: usize,
  /// The hash function used to create digests.
  hasher: H,
}

impl SdObjectEncoder {
  /// Creates a new [`SdObjectEncoder`] with `sha-256` hash function.
  ///
  /// ## Error
  /// Returns [`Error::DeserializationError`] if `object` is not a valid JSON object.
  pub fn new(object: &str) -> Result<SdObjectEncoder<Sha256Hasher>> {
    Ok(SdObjectEncoder {
      object: serde_json::from_str(object).map_err(|e| Error::DeserializationError(e.to_string()))?,
      salt_length: DEFAULT_SALT_SIZE,
      hasher: Sha256Hasher::new(),
    })
  }
}

impl TryFrom<Value> for SdObjectEncoder {
  type Error = crate::Error;

  fn try_from(value: Value) -> std::result::Result<Self, Self::Error> {
    match value {
      Value::Object(object) => Ok(SdObjectEncoder {
        object,
        salt_length: DEFAULT_SALT_SIZE,
        hasher: Sha256Hasher::new(),
      }),
      _ => Err(Error::DataTypeMismatch("expected object".to_owned())),
    }
  }
}

impl<H: Hasher> SdObjectEncoder<H> {
  /// Creates a new [`SdObjectEncoder`] with custom hash function to create digests.
  pub fn with_custom_hasher(object: &str, hasher: H) -> Result<Self> {
    Ok(Self {
      object: serde_json::from_str(object).map_err(|e| Error::DeserializationError(e.to_string()))?,
      salt_length: DEFAULT_SALT_SIZE,
      hasher,
    })
  }

  /// Substitutes a value with the digest of its disclosure.
  /// If no salt is provided, the disclosure will be created with a random salt value.
  ///
  /// The value of the key specified in `path` will be concealed. E.g. for path
  /// `["claim", "subclaim"]` the value of `claim.subclaim` will be concealed.
  ///
  /// ## Error
  /// [`Error::InvalidPath`] if path is invalid or the path slice is empty.
  /// [`Error::DataTypeMismatch`] if existing SD format is invalid.
  ///
  /// ## Note
  /// Use `conceal_array_entry` for values in arrays.
  pub fn conceal(&mut self, path: &[&str], salt: Option<String>) -> Result<Disclosure> {
    // Error if path is not provided.
    if path.is_empty() {
      return Err(Error::InvalidPath("the provided path length is 0".to_string()));
    }

    // Determine salt.
    let salt = salt.unwrap_or(Self::gen_rand(self.salt_length));

    // Obtain the parent of the property specified by the provided path.
    let (target_key, parent_value) = Self::get_target_property_and_its_parent(&mut self.object, path)?;

    // Remove the value from the parent and create a disclosure for it.
    let disclosure = Disclosure::new(
      salt,
      Some(target_key.to_owned()),
      parent_value
        .remove(target_key)
        .ok_or(Error::InvalidPath(format!("{} does not exist", target_key)))?,
    );

    // Hash the disclosure.
    let hash = Utils::digest_b64_url_only_ascii(&self.hasher, disclosure.as_str());

    // Add the hash to the "_sd" array if exists; otherwise, create the array and insert the hash.
    Self::add_digest_to_object(parent_value, hash)?;
    Ok(disclosure)
  }

  /// Substitutes a value within an array with the digest of its disclosure.
  /// If no salt is provided, the disclosure will be created with random salt value.
  ///
  /// `path` is used to specify the array in the object, while `element_index` specifies
  /// the index of the element to be concealed (index start at 0).
  ///
  /// ## Error
  /// [`Error::InvalidPath`] if path is invalid or the path slice is empty.
  /// [`Error::DataTypeMismatch`] if existing SD format is invalid.
  /// [`Error::IndexOutofBounds`] if `element_index` is out of bounds.
  pub fn conceal_array_entry(
    &mut self,
    path: &[&str],
    element_index: usize,
    salt: Option<String>,
  ) -> Result<Disclosure> {
    // Error if path is not provided.
    if path.is_empty() {
      return Err(Error::InvalidPath("the provided path length is 0".to_string()));
    }

    // Determine salt.
    let salt = salt.unwrap_or(Self::gen_rand(self.salt_length));

    // Obtain the parent of the property specified by the provided path.
    let (target_key, parent_value) = Self::get_target_property_and_its_parent(&mut self.object, path)?;

    let array = parent_value
      .get_mut(target_key)
      .ok_or(Error::InvalidPath(format!("{} does not exist", target_key)))?
      .as_array_mut()
      .ok_or(Error::InvalidPath(format!("{} is not an array", target_key)))?;

    // Get array element, calculate digest of the disclosure and replace the element with the object
    // of form "{"...": "<digest>"}".
    if let Some(element_value) = array.get_mut(element_index) {
      let disclosure = Disclosure::new(salt, None, element_value.clone());
      let hash = Utils::digest_b64_url_only_ascii(&self.hasher, disclosure.as_str());
      let tripledot = json!({ARRAY_DIGEST_KEY: hash});
      *element_value = tripledot;
      Ok(disclosure)
    } else {
      Err(Error::IndexOutofBounds(element_index))
    }
  }

  fn get_target_property_and_its_parent<'a, 'b>(
    json: &'a mut Map<String, Value>,
    path: &'b [&str],
  ) -> Result<(&'b str, &'a mut Map<String, Value>)> {
    let mut parent_value = json;
    let mut target_property = path[0];
    for index in 1..path.len() {
      match parent_value
        .get(target_property)
        .ok_or(Error::InvalidPath(format!("{} does not exist", target_property)))?
      {
        Value::Object(_) => {
          parent_value = parent_value
            .get_mut(path[index - 1])
            .ok_or(Error::InvalidPath(format!("{} does not exist", path[index - 1])))?
            .as_object_mut()
            .ok_or(Error::InvalidPath(format!("{} is not an object", path[index - 1])))?;
          target_property = path[index];
        }
        _ => return Err(Error::InvalidPath(format!("{} is not an object", target_property))),
      }
    }
    Ok((target_property, parent_value))
  }

  /// Adds the `_sd_alg` property to the top level of the object.
  /// The value is taken from the [`crate::Hasher::alg_name`] implementation.
  pub fn add_sd_alg_property(&mut self) -> Option<Value> {
    self
      .object
      .insert("_sd_alg".to_string(), Value::String(self.hasher.alg_name().to_string()))
  }

  /// Returns the modified object as a string.
  pub fn try_to_string(&self) -> Result<String> {
    serde_json::to_string(&self.object)
      .map_err(|_e| Error::Unspecified("error while serializing internal object".to_string()))
  }

  /// Adds a decoy digest to the specified path.
  /// If path is an empty slice, decoys will be added to the top level.
  pub fn add_decoys(&mut self, path: &[&str], number_of_decoys: usize) -> Result<()> {
    for _ in 0..number_of_decoys {
      self.add_decoy(path)?;
    }
    Ok(())
  }

  fn add_decoy(&mut self, path: &[&str]) -> Result<Disclosure> {
    if path.is_empty() {
      let (disclosure, hash) = Self::random_digest(&self.hasher, self.salt_length, true);
      Self::add_digest_to_object(&mut self.object, hash)?;
      Ok(disclosure)
    } else {
      let (target_key, parent_value) = Self::get_target_property_and_its_parent(&mut self.object, path)?;

      let value: &mut Value = parent_value
        .get_mut(target_key)
        .ok_or(Error::InvalidPath(format!("{} does not exist", target_key)))?;

      if let Some(object) = value.as_object_mut() {
        let (disclosure, hash) = Self::random_digest(&self.hasher, self.salt_length, true);
        Self::add_digest_to_object(object, hash)?;
        Ok(disclosure)
      } else if let Some(array) = value.as_array_mut() {
        let (disclosure, hash) = Self::random_digest(&self.hasher, self.salt_length, true);
        let tripledot = json!({ARRAY_DIGEST_KEY: hash});
        array.push(tripledot);
        Ok(disclosure)
      } else {
        Err(Error::InvalidPath(format!(
          "{} is neither an object nor an array",
          target_key
        )))
      }
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
    let hash = Utils::digest_b64_url_only_ascii(hasher, disclosure.as_str());
    (disclosure, hash)
  }

  fn gen_rand(len: usize) -> String {
    // todo: check if random is cryptographically secure.
    rand::distributions::Alphanumeric.sample_string(&mut rand::thread_rng(), len)
  }

  /// Returns a reference to the internal object.
  pub fn object(&self) -> &Map<String, Value> {
    &self.object
  }

  /// Returns a mutable reference to the internal object.
  pub fn object_mut(&mut self) -> &mut Map<String, Value> {
    &mut self.object
  }

  /// Returns the used salt length.
  pub fn salt_length(&self) -> usize {
    self.salt_length
  }

  /// Sets the used salt length.
  ///
  /// ## Warning
  /// If the new value is 0, it will not be set.
  pub fn set_salt_length(&mut self, salt_length: usize) {
    if salt_length > 0 {
      self.salt_length = salt_length
    }
  }
}

#[cfg(test)]
mod test {
  use super::SdObjectEncoder;
  use crate::Error;
  use serde_json::json;
  use serde_json::Value;

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
    encoder.conceal(&["claim1", "abc"], None).unwrap();
    encoder.conceal(&["id"], None).unwrap();
    encoder.add_decoys(&[], 10).unwrap();
    encoder.add_decoys(&["claim2"], 10).unwrap();
    assert!(encoder.object().get("id").is_none());
    assert_eq!(encoder.object.get("_sd").unwrap().as_array().unwrap().len(), 11);
    assert_eq!(encoder.object.get("claim2").unwrap().as_array().unwrap().len(), 12);
  }

  #[test]
  fn errors() {
    let mut encoder = SdObjectEncoder::try_from(object()).unwrap();
    encoder.conceal(&["claim1", "abc"], None).unwrap();
    assert!(matches!(
      encoder.conceal_array_entry(&["claim2"], 2, None).unwrap_err(),
      Error::IndexOutofBounds(2)
    ));
  }

  #[test]
  fn test_wrong_path() {
    let mut encoder = SdObjectEncoder::try_from(object()).unwrap();
    assert!(matches!(
      encoder.conceal(&["claim12"], None).unwrap_err(),
      Error::InvalidPath(_)
    ));
    assert!(matches!(
      encoder.conceal_array_entry(&["claim12"], 0, None).unwrap_err(),
      Error::InvalidPath(_)
    ));
  }
}
