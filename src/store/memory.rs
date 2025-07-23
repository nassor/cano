use super::{StoreResult, StoreTrait, error::StoreError};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Thread-safe in-memory HashMap-based store
///
/// `MemoryStore` provides a thread-safe store solution using
/// a `HashMap` wrapped in `Arc<RwLock<_>>` as the backing store.
/// This is the default store implementation used throughout Cano workflows.
///
/// ## Thread Safety
///
/// The store is fully thread-safe using `Arc<RwLock<_>>` for interior mutability.
/// Multiple readers can access the store concurrently, while writers get exclusive access.
///
/// ## Performance Characteristics
///
/// - **Reads**: Concurrent reads with shared locks
/// - **Writes**: Exclusive writes with write locks
/// - **Memory**: HashMap grows dynamically
/// - **Concurrency**: Optimized for read-heavy workloads
#[derive(Default, Clone)]
pub struct MemoryStore {
    /// Internal HashMap storing the key-value pairs, wrapped in Arc<RwLock<_>> for thread safety
    data: Arc<RwLock<HashMap<String, Box<dyn std::any::Any + Send + Sync>>>>,
}

impl MemoryStore {
    /// Create a new empty MemoryStore instance
    ///
    /// Creates a new thread-safe store instance with an empty HashMap.
    /// This is the most common way to initialize store for workflows.
    ///
    /// # Returns
    /// A new, empty `MemoryStore` instance
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl StoreTrait for MemoryStore {
    fn get<T: 'static + Clone>(&self, key: &str) -> StoreResult<T> {
        let data = self
            .data
            .read()
            .map_err(|_| StoreError::lock_error("Failed to acquire read lock on store"))?;

        match data.get(key) {
            Some(value) => value.downcast_ref::<T>().cloned().ok_or_else(|| {
                StoreError::type_mismatch(format!(
                    "Cannot downcast value for key '{key}' to requested type"
                ))
            }),
            None => Err(StoreError::key_not_found(key)),
        }
    }

    fn put<T: 'static + Send + Sync + Clone>(&self, key: &str, value: T) -> StoreResult<()> {
        let mut data = self
            .data
            .write()
            .map_err(|_| StoreError::lock_error("Failed to acquire write lock on store"))?;

        data.insert(key.to_string(), Box::new(value));
        Ok(())
    }

    fn remove(&self, key: &str) -> StoreResult<()> {
        let mut data = self
            .data
            .write()
            .map_err(|_| StoreError::lock_error("Failed to acquire write lock on store"))?;

        data.remove(key)
            .map(|_| ())
            .ok_or_else(|| StoreError::key_not_found(key))
    }

    fn append<T: 'static + Send + Sync + Clone>(&self, key: &str, item: T) -> StoreResult<()> {
        let mut data = self
            .data
            .write()
            .map_err(|_| StoreError::lock_error("Failed to acquire write lock on store"))?;

        if let Some(existing) = data.get_mut(key) {
            // Try to downcast to Vec<T> and append
            if let Some(vec) = existing.downcast_mut::<Vec<T>>() {
                vec.push(item);
                Ok(())
            } else {
                // Key exists but is not a Vec<T>
                Err(StoreError::append_type_mismatch(key))
            }
        } else {
            // Key doesn't exist, create new Vec<T> with the item
            data.insert(key.to_string(), Box::new(vec![item]));
            Ok(())
        }
    }

    fn keys(&self) -> StoreResult<Box<dyn Iterator<Item = String> + '_>> {
        let data = self
            .data
            .read()
            .map_err(|_| StoreError::lock_error("Failed to acquire read lock on store"))?;

        let keys: Vec<String> = data.keys().cloned().collect();
        Ok(Box::new(keys.into_iter()))
    }

    fn len(&self) -> StoreResult<usize> {
        let data = self
            .data
            .read()
            .map_err(|_| StoreError::lock_error("Failed to acquire read lock on store"))?;

        Ok(data.len())
    }

    fn clear(&self) -> StoreResult<()> {
        let mut data = self
            .data
            .write()
            .map_err(|_| StoreError::lock_error("Failed to acquire write lock on store"))?;

        data.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_new_store() {
        let store = MemoryStore::new();
        assert_eq!(store.len().unwrap(), 0);
        assert!(store.is_empty().unwrap());
    }

    #[test]
    fn test_default_store() {
        let store = MemoryStore::default();
        assert_eq!(store.len().unwrap(), 0);
        assert!(store.is_empty().unwrap());
    }

    #[test]
    fn test_put_and_get_string() {
        let store = MemoryStore::new();
        let key = "test_string";
        let value = "Hello, World!".to_string();

        // Put value
        store.put(key, value.clone()).unwrap();

        // Get value
        let retrieved: String = store.get(key).unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_put_and_get_integer() {
        let store = MemoryStore::new();
        let key = "test_int";
        let value = 42i32;

        store.put(key, value).unwrap();
        let retrieved: i32 = store.get(key).unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_put_and_get_vector() {
        let store = MemoryStore::new();
        let key = "test_vec";
        let value = vec![1, 2, 3, 4, 5];

        store.put(key, value.clone()).unwrap();
        let retrieved: Vec<i32> = store.get(key).unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_put_and_get_custom_struct() {
        #[derive(Debug, Clone, PartialEq)]
        struct TestData {
            id: u32,
            name: String,
        }

        let store = MemoryStore::new();
        let key = "test_struct";
        let value = TestData {
            id: 123,
            name: "Test".to_string(),
        };

        store.put(key, value.clone()).unwrap();
        let retrieved: TestData = store.get(key).unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_get_nonexistent_key() {
        let store = MemoryStore::new();
        let result: Result<String, StoreError> = store.get("nonexistent");

        assert!(result.is_err());
        match result.unwrap_err() {
            StoreError::KeyNotFound(msg) => {
                assert_eq!(msg, "Key 'nonexistent' not found in store")
            }
            _ => panic!("Expected KeyNotFound error"),
        }
    }

    #[test]
    fn test_type_mismatch() {
        let store = MemoryStore::new();
        let key = "test_type";

        // Store as string
        store.put(key, "hello".to_string()).unwrap();

        // Try to retrieve as integer
        let result: Result<i32, StoreError> = store.get(key);
        assert!(result.is_err());
        match result.unwrap_err() {
            StoreError::TypeMismatch(_) => (),
            _ => panic!("Expected TypeMismatch error"),
        }
    }

    #[test]
    fn test_remove_existing_key() {
        let store = MemoryStore::new();
        let key = "test_remove";
        let value = "to_be_removed".to_string();

        // Put and verify
        store.put(key, value).unwrap();
        assert!(store.get::<String>(key).is_ok());

        // Remove and verify
        store.remove(key).unwrap();
        assert!(store.get::<String>(key).is_err());
    }

    #[test]
    fn test_remove_nonexistent_key() {
        let store = MemoryStore::new();
        let result = store.remove("nonexistent");

        assert!(result.is_err());
        match result.unwrap_err() {
            StoreError::KeyNotFound(msg) => {
                assert_eq!(msg, "Key 'nonexistent' not found in store")
            }
            _ => panic!("Expected KeyNotFound error"),
        }
    }

    #[test]
    fn test_delete_alias() {
        let store = MemoryStore::new();
        let key = "test_delete";
        let value = "to_be_deleted".to_string();

        // Put and verify
        store.put(key, value).unwrap();
        assert!(store.get::<String>(key).is_ok());

        // Delete using alias and verify
        store.delete(key).unwrap();
        assert!(store.get::<String>(key).is_err());
    }

    #[test]
    fn test_append_to_new_key() {
        let store = MemoryStore::new();
        let key = "test_append_new";
        let item = "first_item".to_string();

        // Append to non-existent key should create new Vec
        store.append(key, item.clone()).unwrap();

        let retrieved: Vec<String> = store.get(key).unwrap();
        assert_eq!(retrieved, vec![item]);
    }

    #[test]
    fn test_append_to_existing_vector() {
        let store = MemoryStore::new();
        let key = "test_append_existing";
        let initial_vec = vec!["first".to_string(), "second".to_string()];

        // Put initial vector
        store.put(key, initial_vec.clone()).unwrap();

        // Append new item
        let new_item = "third".to_string();
        store.append(key, new_item.clone()).unwrap();

        // Verify the vector was updated
        let retrieved: Vec<String> = store.get(key).unwrap();
        let expected = vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ];
        assert_eq!(retrieved, expected);
    }

    #[test]
    fn test_append_to_non_vector() {
        let store = MemoryStore::new();
        let key = "test_append_error";

        // Store a non-vector value
        store.put(key, "not_a_vector".to_string()).unwrap();

        // Try to append to it
        let result = store.append(key, "item".to_string());
        assert!(result.is_err());
        match result.unwrap_err() {
            StoreError::AppendTypeMismatch(msg) => assert_eq!(
                msg,
                "Cannot append to key 'test_append_error': existing value is not a Vec<T>"
            ),
            _ => panic!("Expected AppendTypeMismatch error"),
        }
    }

    #[test]
    fn test_keys_empty_store() {
        let store = MemoryStore::new();
        let keys: Vec<String> = store.keys().unwrap().collect();
        assert!(keys.is_empty());
    }

    #[test]
    fn test_keys_with_data() {
        let store = MemoryStore::new();

        // Add some data
        store.put("key1", "value1".to_string()).unwrap();
        store.put("key2", 42i32).unwrap();
        store.put("key3", vec![1, 2, 3]).unwrap();

        let keys: Vec<String> = store.keys().unwrap().collect();
        assert_eq!(keys.len(), 3);

        // Check that all keys are present (order might vary)
        assert!(keys.contains(&"key1".to_string()));
        assert!(keys.contains(&"key2".to_string()));
        assert!(keys.contains(&"key3".to_string()));
    }

    #[test]
    fn test_len_and_is_empty() {
        let store = MemoryStore::new();

        // Initially empty
        assert_eq!(store.len().unwrap(), 0);
        assert!(store.is_empty().unwrap());

        // Add one item
        store.put("key1", "value1".to_string()).unwrap();
        assert_eq!(store.len().unwrap(), 1);
        assert!(!store.is_empty().unwrap());

        // Add more items
        store.put("key2", 42i32).unwrap();
        store.put("key3", vec![1, 2, 3]).unwrap();
        assert_eq!(store.len().unwrap(), 3);
        assert!(!store.is_empty().unwrap());

        // Remove one item
        store.remove("key2").unwrap();
        assert_eq!(store.len().unwrap(), 2);
        assert!(!store.is_empty().unwrap());
    }

    #[test]
    fn test_clear() {
        let store = MemoryStore::new();

        // Add some data
        store.put("key1", "value1".to_string()).unwrap();
        store.put("key2", 42i32).unwrap();
        store.put("key3", vec![1, 2, 3]).unwrap();
        assert_eq!(store.len().unwrap(), 3);

        // Clear the store
        store.clear().unwrap();
        assert_eq!(store.len().unwrap(), 0);
        assert!(store.is_empty().unwrap());

        // Verify data is actually gone
        assert!(store.get::<String>("key1").is_err());
        assert!(store.get::<i32>("key2").is_err());
        assert!(store.get::<Vec<i32>>("key3").is_err());
    }

    #[test]
    fn test_overwrite_existing_key() {
        let store = MemoryStore::new();
        let key = "test_overwrite";

        // Store initial value
        store.put(key, "initial".to_string()).unwrap();
        assert_eq!(store.get::<String>(key).unwrap(), "initial");

        // Overwrite with new value
        store.put(key, "overwritten".to_string()).unwrap();
        assert_eq!(store.get::<String>(key).unwrap(), "overwritten");

        // Verify length didn't change
        assert_eq!(store.len().unwrap(), 1);
    }

    #[test]
    fn test_overwrite_with_different_type() {
        let store = MemoryStore::new();
        let key = "test_type_overwrite";

        // Store string value
        store.put(key, "string_value".to_string()).unwrap();
        assert_eq!(store.get::<String>(key).unwrap(), "string_value");

        // Overwrite with integer value
        store.put(key, 42i32).unwrap();
        assert_eq!(store.get::<i32>(key).unwrap(), 42);

        // Verify old type no longer accessible
        assert!(store.get::<String>(key).is_err());
        assert_eq!(store.len().unwrap(), 1);
    }

    #[test]
    fn test_clone_store() {
        let store1 = MemoryStore::new();
        store1.put("key1", "value1".to_string()).unwrap();

        // Clone the store
        let store2 = store1.clone();

        // Both stores should share the same data
        assert_eq!(store2.get::<String>("key1").unwrap(), "value1");

        // Modifying one should affect the other (shared Arc)
        store2.put("key2", "value2".to_string()).unwrap();
        assert_eq!(store1.get::<String>("key2").unwrap(), "value2");
        assert_eq!(store1.len().unwrap(), 2);
        assert_eq!(store2.len().unwrap(), 2);
    }

    #[test]
    fn test_thread_safety_concurrent_reads() {
        let store = Arc::new(MemoryStore::new());

        // Pre-populate store
        store.put("shared_key", "shared_value".to_string()).unwrap();

        let mut handles = vec![];

        // Spawn multiple reader threads
        for i in 0..10 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                for _ in 0..100 {
                    let value: String = store_clone.get("shared_key").unwrap();
                    assert_eq!(value, "shared_value");

                    // Also test len() for concurrent reads
                    let len = store_clone.len().unwrap();
                    assert!(len > 0);
                }
                i
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_thread_safety_concurrent_writes() {
        let store = Arc::new(MemoryStore::new());
        let mut handles = vec![];

        // Spawn multiple writer threads
        for i in 0..10 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                for j in 0..10 {
                    let key = format!("key_{i}_{j}");
                    let value = format!("value_{i}_{j}");
                    store_clone.put(&key, value).unwrap();
                }
                i
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify all values were written
        assert_eq!(store.len().unwrap(), 100); // 10 threads * 10 writes each

        // Verify some values
        assert_eq!(store.get::<String>("key_0_0").unwrap(), "value_0_0");
        assert_eq!(store.get::<String>("key_5_7").unwrap(), "value_5_7");
        assert_eq!(store.get::<String>("key_9_9").unwrap(), "value_9_9");
    }

    #[test]
    fn test_thread_safety_mixed_operations() {
        let store = Arc::new(MemoryStore::new());

        // Pre-populate with some data
        for i in 0..50 {
            store.put(&format!("initial_{i}"), i).unwrap();
        }

        let mut handles = vec![];

        // Spawn reader threads
        for _ in 0..5 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                for i in 0..50 {
                    if let Ok(value) = store_clone.get::<i32>(&format!("initial_{i}")) {
                        assert_eq!(value, i);
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            });
            handles.push(handle);
        }

        // Spawn writer threads
        for thread_id in 0..3 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                for i in 0..20 {
                    let key = format!("new_{thread_id}_{i}");
                    let value = format!("new_value_{thread_id}_{i}");
                    store_clone.put(&key, value).unwrap();
                    thread::sleep(Duration::from_millis(1));
                }
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify the store is in a consistent state
        let final_len = store.len().unwrap();
        assert_eq!(final_len, 110); // 50 initial + 60 new (3 threads * 20 each)
    }

    #[test]
    fn test_append_thread_safety() {
        let store = Arc::new(MemoryStore::new());
        let mut handles = vec![];

        // Spawn multiple threads that append to the same vector
        for thread_id in 0..5 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                for i in 0..10 {
                    let value = format!("thread_{thread_id}_item_{i}");
                    store_clone.append("shared_vector", value).unwrap();
                }
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify the final vector contains all items
        let final_vec: Vec<String> = store.get("shared_vector").unwrap();
        assert_eq!(final_vec.len(), 50); // 5 threads * 10 items each

        // Verify all expected items are present
        for thread_id in 0..5 {
            for i in 0..10 {
                let expected_value = format!("thread_{thread_id}_item_{i}");
                assert!(final_vec.contains(&expected_value));
            }
        }
    }

    #[test]
    fn test_complex_data_types() {
        #[derive(Debug, Clone, PartialEq)]
        struct ComplexData {
            numbers: Vec<i32>,
            text: String,
            optional: Option<String>,
            nested: std::collections::HashMap<String, i32>,
        }

        let store = MemoryStore::new();
        let mut nested = std::collections::HashMap::new();
        nested.insert("nested_key".to_string(), 42);

        let complex_data = ComplexData {
            numbers: vec![1, 2, 3, 4, 5],
            text: "Complex data structure".to_string(),
            optional: Some("Some value".to_string()),
            nested,
        };

        // Store and retrieve complex data
        store.put("complex", complex_data.clone()).unwrap();
        let retrieved: ComplexData = store.get("complex").unwrap();
        assert_eq!(retrieved, complex_data);
    }

    #[test]
    fn test_large_data_handling() {
        let store = MemoryStore::new();

        // Create a large vector
        let large_vec: Vec<i32> = (0..10000).collect();

        // Store and retrieve large data
        store.put("large_data", large_vec.clone()).unwrap();
        let retrieved: Vec<i32> = store.get("large_data").unwrap();
        assert_eq!(retrieved, large_vec);
        assert_eq!(retrieved.len(), 10000);
    }
}
