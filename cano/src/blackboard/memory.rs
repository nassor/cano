use super::{BlackboardResult, error::BlackboardError};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Thread-safe in-memory typed key-value blackboard
///
/// `Blackboard` provides a thread-safe shared map using
/// a `HashMap` wrapped in `Arc<parking_lot::RwLock<_>>` as the backing storage.
/// This is the default blackboard implementation used throughout Cano workflows.
///
/// ## Thread Safety
///
/// The blackboard is fully thread-safe using `Arc<parking_lot::RwLock<_>>` for interior
/// mutability. Multiple readers can access the blackboard concurrently, while writers get
/// exclusive access. `parking_lot::RwLock` is used instead of `std::sync::RwLock` for
/// faster lock acquisition and an infallible (non-poisonable) API.
///
/// ## Performance Characteristics
///
/// - **Reads**: Concurrent reads with shared locks
/// - **Writes**: Exclusive writes with write locks
/// - **Memory**: HashMap grows dynamically
/// - **Concurrency**: Optimized for read-heavy workloads
#[derive(Default, Clone)]
pub struct Blackboard {
    /// Internal HashMap storing the key-value pairs, wrapped in Arc<RwLock<_>> for thread safety
    data: Arc<RwLock<HashMap<String, Arc<dyn std::any::Any + Send + Sync>>>>,
}

impl Blackboard {
    /// Create a new empty Blackboard instance
    ///
    /// Creates a new thread-safe blackboard instance with an empty HashMap.
    /// This is the most common way to initialize a blackboard for workflows.
    ///
    /// # Returns
    /// A new, empty `Blackboard` instance
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get a shared reference to a value in the blackboard (Zero-Copy)
    ///
    /// This method returns an `Arc<T>` pointing to the data in the blackboard.
    /// This avoids cloning the data, making it much more efficient for large objects.
    ///
    /// # Type Parameters
    /// - `TState`: The expected type of the value
    ///
    /// # Arguments
    /// - `key`: The key to look up
    ///
    /// # Returns
    /// - `Ok(Arc<TState>)` if the key exists and the type matches
    /// - `Err(BlackboardError::KeyNotFound)` if the key doesn't exist
    /// - `Err(BlackboardError::TypeMismatch)` if the value is not of type `TState`
    pub fn get_shared<TState: 'static + Send + Sync>(
        &self,
        key: &str,
    ) -> BlackboardResult<Arc<TState>> {
        let data = self.data.read();

        match data.get(key) {
            Some(value) => value.clone().downcast::<TState>().map_err(|_| {
                BlackboardError::type_mismatch(format!(
                    "Cannot downcast value for key '{key}' to requested type"
                ))
            }),
            None => Err(BlackboardError::key_not_found(key)),
        }
    }
    /// Retrieve a typed value by key.
    ///
    /// Returns a clone of the stored value if the key exists and the value can be
    /// downcast to `TState`.
    #[inline]
    pub fn get<TState: 'static + Clone>(&self, key: &str) -> BlackboardResult<TState> {
        let data = self.data.read();

        match data.get(key) {
            Some(value) => value.downcast_ref::<TState>().cloned().ok_or_else(|| {
                BlackboardError::type_mismatch(format!(
                    "Cannot downcast value for key '{key}' to requested type"
                ))
            }),
            None => Err(BlackboardError::key_not_found(key)),
        }
    }

    /// Store a typed value by key.
    ///
    /// Inserts or replaces the value at `key`. The stored value must be safe to
    /// share across async tasks, but it does not need to implement `Clone`.
    #[inline]
    pub fn put<TState: 'static + Send + Sync>(&self, key: &str, value: TState) {
        self.data.write().insert(key.to_string(), Arc::new(value));
    }

    /// Remove a value by key.
    ///
    /// Removing a missing key is a silent no-op.
    pub fn remove(&self, key: &str) {
        self.data.write().remove(key);
    }

    /// Append an item to an existing `Vec<TState>` value.
    ///
    /// If the key does not exist, creates a new `Vec<TState>` with `item` as
    /// its first element. Returns an error if the key exists but holds a
    /// different type.
    pub fn append<TState: 'static + Send + Sync + Clone>(
        &self,
        key: &str,
        item: TState,
    ) -> BlackboardResult<()> {
        // Acquire write lock once and perform all operations under it to avoid TOCTOU race
        let mut data = self.data.write();

        if let Some(existing) = data.get_mut(key) {
            // Fast path: key exists — swap in a placeholder so we can take ownership
            // of the original Arc without a remove + re-insert round-trip.
            let placeholder: Arc<dyn std::any::Any + Send + Sync> = Arc::new(());
            let arc = std::mem::replace(existing, placeholder);
            match arc.downcast::<Vec<TState>>() {
                Ok(arc_vec) => {
                    // Try to take ownership of the Vec to avoid a clone when no
                    // other Arc references exist (the common case when the value
                    // was only put/appended and never retrieved via get_shared).
                    let mut vec = match Arc::try_unwrap(arc_vec) {
                        Ok(v) => v,
                        Err(shared) => (*shared).clone(),
                    };
                    vec.push(item);
                    // Put the updated Vec back in-place — no HashMap remove/insert.
                    *existing = Arc::new(vec);
                    Ok(())
                }
                Err(other) => {
                    // Type mismatch — restore the original Arc, return a non-destructive error.
                    *existing = other;
                    Err(BlackboardError::append_type_mismatch(key))
                }
            }
        } else {
            data.insert(key.to_string(), Arc::new(vec![item]));
            Ok(())
        }
    }

    /// Check whether the blackboard contains the given key.
    pub fn contains_key(&self, key: &str) -> bool {
        self.data.read().contains_key(key)
    }

    /// Get all keys currently stored.
    pub fn keys(&self) -> Vec<Arc<str>> {
        self.data
            .read()
            .keys()
            .map(|k| Arc::from(k.as_str()))
            .collect()
    }

    /// Get the number of key-value pairs in the blackboard.
    pub fn len(&self) -> usize {
        self.data.read().len()
    }

    /// Check whether the blackboard is empty.
    pub fn is_empty(&self) -> bool {
        self.data.read().is_empty()
    }

    /// Clear all data from the blackboard.
    pub fn clear(&self) {
        self.data.write().clear();
    }
}

impl crate::resource::Resource for Blackboard {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Resource;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[tokio::test]
    async fn test_blackboard_resource_lifecycle() {
        let board = Blackboard::new();
        assert!(board.setup().await.is_ok());
        assert!(board.teardown().await.is_ok());
    }

    #[test]
    fn test_new_blackboard() {
        let board = Blackboard::new();
        assert_eq!(board.len(), 0);
        assert!(board.is_empty());
    }

    #[test]
    fn test_default_blackboard() {
        let board = Blackboard::default();
        assert_eq!(board.len(), 0);
        assert!(board.is_empty());
    }

    #[test]
    fn test_put_and_get_string() {
        let board = Blackboard::new();
        let key = "test_string";
        let value = "Hello, World!".to_string();

        // Put value
        board.put(key, value.clone());

        // Get value
        let retrieved: String = board.get(key).unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_put_and_get_integer() {
        let board = Blackboard::new();
        let key = "test_int";
        let value = 42i32;

        board.put(key, value);
        let retrieved: i32 = board.get(key).unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_put_and_get_vector() {
        let board = Blackboard::new();
        let key = "test_vec";
        let value = vec![1, 2, 3, 4, 5];

        board.put(key, value.clone());
        let retrieved: Vec<i32> = board.get(key).unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_put_and_get_custom_struct() {
        #[derive(Debug, Clone, PartialEq)]
        struct TestData {
            id: u32,
            name: String,
        }

        let board = Blackboard::new();
        let key = "test_struct";
        let value = TestData {
            id: 123,
            name: "Test".to_string(),
        };

        board.put(key, value.clone());
        let retrieved: TestData = board.get(key).unwrap();
        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_get_nonexistent_key() {
        let board = Blackboard::new();
        let result: Result<String, BlackboardError> = board.get("nonexistent");

        assert!(result.is_err());
        match result.unwrap_err() {
            BlackboardError::KeyNotFound(msg) => {
                assert_eq!(msg, "Key 'nonexistent' not found in blackboard")
            }
            _ => panic!("Expected KeyNotFound error"),
        }
    }

    #[test]
    fn test_type_mismatch() {
        let board = Blackboard::new();
        let key = "test_type";

        // Store as string
        board.put(key, "hello".to_string());

        // Try to retrieve as integer
        let result: Result<i32, BlackboardError> = board.get(key);
        assert!(result.is_err());
        match result.unwrap_err() {
            BlackboardError::TypeMismatch(_) => (),
            _ => panic!("Expected TypeMismatch error"),
        }
    }

    #[test]
    fn test_remove_existing_key() {
        let board = Blackboard::new();
        let key = "test_remove";
        let value = "to_be_removed".to_string();

        // Put and verify
        board.put(key, value);
        assert!(board.get::<String>(key).is_ok());

        // Remove and verify
        board.remove(key);
        assert!(board.get::<String>(key).is_err());
    }

    #[test]
    fn test_remove_nonexistent_key() {
        let board = Blackboard::new();
        // Removing a missing key is a silent no-op
        board.remove("nonexistent");
        assert!(board.is_empty());
    }

    #[test]
    fn test_append_to_new_key() {
        let board = Blackboard::new();
        let key = "test_append_new";
        let item = "first_item".to_string();

        // Append to non-existent key should create new Vec
        board.append(key, item.clone()).unwrap();

        let retrieved: Vec<String> = board.get(key).unwrap();
        assert_eq!(retrieved, vec![item]);
    }

    #[test]
    fn test_append_to_existing_vector() {
        let board = Blackboard::new();
        let key = "test_append_existing";
        let initial_vec = vec!["first".to_string(), "second".to_string()];

        // Put initial vector
        board.put(key, initial_vec.clone());

        // Append new item
        let new_item = "third".to_string();
        board.append(key, new_item.clone()).unwrap();

        // Verify the vector was updated
        let retrieved: Vec<String> = board.get(key).unwrap();
        let expected = vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ];
        assert_eq!(retrieved, expected);
    }

    #[test]
    fn test_append_to_non_vector() {
        let board = Blackboard::new();
        let key = "test_append_error";

        // Store a non-vector value
        board.put(key, "not_a_vector".to_string());

        // Try to append to it
        let result = board.append(key, "item".to_string());
        assert!(result.is_err());
        match result.unwrap_err() {
            BlackboardError::AppendTypeMismatch(msg) => assert_eq!(
                msg,
                "Cannot append to key 'test_append_error': existing value is not a Vec<TState>"
            ),
            _ => panic!("Expected AppendTypeMismatch error"),
        }
    }

    #[test]
    fn test_append_non_destructive_on_type_mismatch() {
        let board = Blackboard::new();
        let key = "test_non_destructive";
        let original_value = "original".to_string();

        // Store a non-vector value
        board.put(key, original_value.clone());

        // Try to append with wrong type — should fail
        let result = board.append::<String>(key, "item".to_string());
        assert!(result.is_err());

        // Verify the original value is still in the store (non-destructive error)
        let retrieved: String = board
            .get(key)
            .expect("Value should still exist after append error");
        assert_eq!(
            retrieved, original_value,
            "Original value should be preserved after type mismatch"
        );
    }

    #[test]
    fn test_keys_empty_blackboard() {
        let board = Blackboard::new();
        let keys = board.keys();
        assert!(keys.is_empty());
    }

    #[test]
    fn test_keys_with_data() {
        let board = Blackboard::new();

        // Add some data
        board.put("key1", "value1".to_string());
        board.put("key2", 42i32);
        board.put("key3", vec![1, 2, 3]);

        let keys = board.keys();
        assert_eq!(keys.len(), 3);

        // Check that all keys are present (order might vary).
        // Arc<str> deref-compares to &str, so use iter().any() for containment checks.
        assert!(keys.iter().any(|k| k.as_ref() == "key1"));
        assert!(keys.iter().any(|k| k.as_ref() == "key2"));
        assert!(keys.iter().any(|k| k.as_ref() == "key3"));
    }

    #[test]
    fn test_len_and_is_empty() {
        let board = Blackboard::new();

        // Initially empty
        assert_eq!(board.len(), 0);
        assert!(board.is_empty());

        // Add one item
        board.put("key1", "value1".to_string());
        assert_eq!(board.len(), 1);
        assert!(!board.is_empty());

        // Add more items
        board.put("key2", 42i32);
        board.put("key3", vec![1, 2, 3]);
        assert_eq!(board.len(), 3);
        assert!(!board.is_empty());

        // Remove one item
        board.remove("key2");
        assert_eq!(board.len(), 2);
        assert!(!board.is_empty());
    }

    #[test]
    fn test_clear() {
        let board = Blackboard::new();

        // Add some data
        board.put("key1", "value1".to_string());
        board.put("key2", 42i32);
        board.put("key3", vec![1, 2, 3]);
        assert_eq!(board.len(), 3);

        // Clear the store
        board.clear();
        assert_eq!(board.len(), 0);
        assert!(board.is_empty());

        // Verify data is actually gone
        assert!(board.get::<String>("key1").is_err());
        assert!(board.get::<i32>("key2").is_err());
        assert!(board.get::<Vec<i32>>("key3").is_err());
    }

    #[test]
    fn test_overwrite_existing_key() {
        let board = Blackboard::new();
        let key = "test_overwrite";

        // Store initial value
        board.put(key, "initial".to_string());
        assert_eq!(board.get::<String>(key).unwrap(), "initial");

        // Overwrite with new value
        board.put(key, "overwritten".to_string());
        assert_eq!(board.get::<String>(key).unwrap(), "overwritten");

        // Verify length didn't change
        assert_eq!(board.len(), 1);
    }

    #[test]
    fn test_overwrite_with_different_type() {
        let board = Blackboard::new();
        let key = "test_type_overwrite";

        // Store string value
        board.put(key, "string_value".to_string());
        assert_eq!(board.get::<String>(key).unwrap(), "string_value");

        // Overwrite with integer value
        board.put(key, 42i32);
        assert_eq!(board.get::<i32>(key).unwrap(), 42);

        // Verify old type no longer accessible
        assert!(board.get::<String>(key).is_err());
        assert_eq!(board.len(), 1);
    }

    #[test]
    fn test_clone_blackboard() {
        let board1 = Blackboard::new();
        board1.put("key1", "value1".to_string());

        // Clone the store
        let board2 = board1.clone();

        // Both stores should share the same data
        assert_eq!(board2.get::<String>("key1").unwrap(), "value1");

        // Modifying one should affect the other (shared Arc)
        board2.put("key2", "value2".to_string());
        assert_eq!(board1.get::<String>("key2").unwrap(), "value2");
        assert_eq!(board1.len(), 2);
        assert_eq!(board2.len(), 2);
    }

    #[test]
    fn test_thread_safety_concurrent_reads() {
        let board = Arc::new(Blackboard::new());

        // Pre-populate store
        board.put("shared_key", "shared_value".to_string());

        let mut handles = vec![];

        // Spawn multiple reader threads
        for i in 0..10 {
            let board_clone = Arc::clone(&board);
            let handle = thread::spawn(move || {
                for _ in 0..100 {
                    let value: String = board_clone.get("shared_key").unwrap();
                    assert_eq!(value, "shared_value");

                    // Also test len() for concurrent reads
                    let len = board_clone.len();
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
        let board = Arc::new(Blackboard::new());
        let mut handles = vec![];

        // Spawn multiple writer threads
        for i in 0..10 {
            let board_clone = Arc::clone(&board);
            let handle = thread::spawn(move || {
                for j in 0..10 {
                    let key = format!("key_{i}_{j}");
                    let value = format!("value_{i}_{j}");
                    board_clone.put(&key, value);
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
        assert_eq!(board.len(), 100); // 10 threads * 10 writes each

        // Verify some values
        assert_eq!(board.get::<String>("key_0_0").unwrap(), "value_0_0");
        assert_eq!(board.get::<String>("key_5_7").unwrap(), "value_5_7");
        assert_eq!(board.get::<String>("key_9_9").unwrap(), "value_9_9");
    }

    #[test]
    fn test_thread_safety_mixed_operations() {
        let board = Arc::new(Blackboard::new());

        // Pre-populate with some data
        for i in 0..50 {
            board.put(&format!("initial_{i}"), i);
        }

        let mut handles = vec![];

        // Spawn reader threads
        for _ in 0..5 {
            let board_clone = Arc::clone(&board);
            let handle = thread::spawn(move || {
                for i in 0..50 {
                    if let Ok(value) = board_clone.get::<i32>(&format!("initial_{i}")) {
                        assert_eq!(value, i);
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            });
            handles.push(handle);
        }

        // Spawn writer threads
        for thread_id in 0..3 {
            let board_clone = Arc::clone(&board);
            let handle = thread::spawn(move || {
                for i in 0..20 {
                    let key = format!("new_{thread_id}_{i}");
                    let value = format!("new_value_{thread_id}_{i}");
                    board_clone.put(&key, value);
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
        let final_len = board.len();
        assert_eq!(final_len, 110); // 50 initial + 60 new (3 threads * 20 each)
    }

    #[test]
    fn test_append_thread_safety() {
        let board = Arc::new(Blackboard::new());
        let mut handles = vec![];

        // Spawn multiple threads that append to the same vector
        for thread_id in 0..5 {
            let board_clone = Arc::clone(&board);
            let handle = thread::spawn(move || {
                for i in 0..10 {
                    let value = format!("thread_{thread_id}_item_{i}");
                    board_clone.append("shared_vector", value).unwrap();
                }
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify the final vector contains all items
        let final_vec: Vec<String> = board.get("shared_vector").unwrap();
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

        let board = Blackboard::new();
        let mut nested = std::collections::HashMap::new();
        nested.insert("nested_key".to_string(), 42);

        let complex_data = ComplexData {
            numbers: vec![1, 2, 3, 4, 5],
            text: "Complex data structure".to_string(),
            optional: Some("Some value".to_string()),
            nested,
        };

        // Store and retrieve complex data
        board.put("complex", complex_data.clone());
        let retrieved: ComplexData = board.get("complex").unwrap();
        assert_eq!(retrieved, complex_data);
    }

    #[test]
    fn test_large_data_handling() {
        let board = Blackboard::new();

        // Create a large vector
        let large_vec: Vec<i32> = (0..10000).collect();

        // Store and retrieve large data
        board.put("large_data", large_vec.clone());
        let retrieved: Vec<i32> = board.get("large_data").unwrap();
        assert_eq!(retrieved, large_vec);
        assert_eq!(retrieved.len(), 10000);
    }

    #[test]
    fn test_contains_key_present() {
        let board = Blackboard::new();
        board.put("exists", 42i32);
        assert!(board.contains_key("exists"));
    }

    #[test]
    fn test_contains_key_absent() {
        let board = Blackboard::new();
        assert!(!board.contains_key("missing"));
    }

    #[test]
    fn test_contains_key_after_remove() {
        let board = Blackboard::new();
        board.put("key", "val".to_string());
        assert!(board.contains_key("key"));
        board.remove("key");
        assert!(!board.contains_key("key"));
    }

    #[test]
    fn test_get_shared_returns_arc() {
        let board = Blackboard::new();
        board.put("msg", "hello".to_string());

        let arc1: Arc<String> = board.get_shared("msg").unwrap();
        let arc2: Arc<String> = board.get_shared("msg").unwrap();

        assert_eq!(*arc1, "hello");
        assert!(
            Arc::ptr_eq(&arc1, &arc2),
            "get_shared must return clones of the same Arc, not fresh allocations"
        );
    }

    #[test]
    fn test_get_shared_missing_key() {
        let board = Blackboard::new();
        let result: BlackboardResult<Arc<String>> = board.get_shared("nope");
        assert!(result.is_err());
        match result.unwrap_err() {
            BlackboardError::KeyNotFound(_) => (),
            _ => panic!("Expected KeyNotFound error"),
        }
    }

    #[test]
    fn test_get_shared_and_put_do_not_require_clone() {
        // Positive path: store and retrieve a Clone type through the zero-copy API.
        let board = Blackboard::new();
        board.put("n", 42u32);
        let got: Arc<u32> = board.get_shared("n").unwrap();
        assert_eq!(*got, 42);

        // Explicit non-`Clone` type: proves `put` and `get_shared` only require
        // values that can safely be shared across tasks.
        struct NotClone(#[allow(dead_code)] u32);
        board.put("not_clone", NotClone(7));
        let got: Arc<NotClone> = board.get_shared("not_clone").unwrap();
        assert_eq!(got.0, 7);
    }
}
