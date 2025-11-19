#[cfg(test)]
mod tests {
    #[test]
    fn test_basic_math() {
        assert_eq!(2 + 2, 4);
    }
    
    #[test]
    fn test_string_operations() {
        let mut s = String::from("Hello");
        s.push_str(", world!");
        assert_eq!(s, "Hello, world!");
    }
}