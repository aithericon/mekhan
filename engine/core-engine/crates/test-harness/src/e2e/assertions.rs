//! Assertion helpers for Petri net state verification.

use petri_domain::{Marking, PlaceId, TokenColor};

/// Extension trait for asserting on Marking state.
///
/// Provides convenient assertion methods for verifying token distribution
/// after scenario execution.
///
/// # Example
///
/// ```ignore
/// use petri_test_harness::e2e::MarkingAssertions;
///
/// let marking = service.get_marking();
/// marking.assert_token_count(&place_id, 3);
/// marking.assert_empty(&other_place_id);
/// ```
pub trait MarkingAssertions {
    /// Assert exact token count in a place.
    fn assert_token_count(&self, place_id: &PlaceId, expected: usize);

    /// Assert place is empty (has no tokens).
    fn assert_empty(&self, place_id: &PlaceId);

    /// Assert place has at least N tokens.
    fn assert_at_least(&self, place_id: &PlaceId, min: usize);

    /// Assert all specified places are empty.
    fn assert_all_empty(&self, place_ids: &[&PlaceId]);

    /// Assert a token with specific color exists in place.
    fn assert_contains_token(&self, place_id: &PlaceId, expected: &TokenColor);

    /// Get token count for a place.
    fn token_count(&self, place_id: &PlaceId) -> usize;
}

impl MarkingAssertions for Marking {
    fn assert_token_count(&self, place_id: &PlaceId, expected: usize) {
        let actual = self.tokens_at(place_id).len();
        assert_eq!(
            actual, expected,
            "Place {:?}: expected {} tokens, found {}",
            place_id, expected, actual
        );
    }

    fn assert_empty(&self, place_id: &PlaceId) {
        self.assert_token_count(place_id, 0);
    }

    fn assert_at_least(&self, place_id: &PlaceId, min: usize) {
        let actual = self.tokens_at(place_id).len();
        assert!(
            actual >= min,
            "Place {:?}: expected at least {} tokens, found {}",
            place_id,
            min,
            actual
        );
    }

    fn assert_all_empty(&self, place_ids: &[&PlaceId]) {
        for place_id in place_ids {
            self.assert_empty(place_id);
        }
    }

    fn assert_contains_token(&self, place_id: &PlaceId, expected: &TokenColor) {
        let tokens = self.tokens_at(place_id);
        assert!(
            tokens.iter().any(|t| &t.color == expected),
            "Place {:?}: expected token with color {:?} not found. Found: {:?}",
            place_id,
            expected,
            tokens.iter().map(|t| &t.color).collect::<Vec<_>>()
        );
    }

    fn token_count(&self, place_id: &PlaceId) -> usize {
        self.tokens_at(place_id).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::Token;

    fn make_marking_with_tokens(place_id: PlaceId, count: usize) -> Marking {
        let mut marking = Marking::new();
        for _ in 0..count {
            marking.add_token(place_id.clone(), Token::new_unit());
        }
        marking
    }

    #[test]
    fn test_assert_token_count_success() {
        let place_id = PlaceId::new();
        let marking = make_marking_with_tokens(place_id.clone(), 3);
        marking.assert_token_count(&place_id, 3);
    }

    #[test]
    #[should_panic(expected = "expected 5 tokens, found 3")]
    fn test_assert_token_count_failure() {
        let place_id = PlaceId::new();
        let marking = make_marking_with_tokens(place_id.clone(), 3);
        marking.assert_token_count(&place_id, 5);
    }

    #[test]
    fn test_assert_empty_success() {
        let place_id = PlaceId::new();
        let marking = Marking::new();
        marking.assert_empty(&place_id);
    }

    #[test]
    #[should_panic(expected = "expected 0 tokens, found 1")]
    fn test_assert_empty_failure() {
        let place_id = PlaceId::new();
        let marking = make_marking_with_tokens(place_id.clone(), 1);
        marking.assert_empty(&place_id);
    }

    #[test]
    fn test_assert_at_least_success() {
        let place_id = PlaceId::new();
        let marking = make_marking_with_tokens(place_id.clone(), 5);
        marking.assert_at_least(&place_id, 3);
        marking.assert_at_least(&place_id, 5);
    }

    #[test]
    #[should_panic(expected = "expected at least 10 tokens, found 5")]
    fn test_assert_at_least_failure() {
        let place_id = PlaceId::new();
        let marking = make_marking_with_tokens(place_id.clone(), 5);
        marking.assert_at_least(&place_id, 10);
    }

    #[test]
    fn test_token_count() {
        let place_id = PlaceId::new();
        let marking = make_marking_with_tokens(place_id.clone(), 7);
        assert_eq!(marking.token_count(&place_id), 7);
    }

    #[test]
    fn test_token_count_empty() {
        let place_id = PlaceId::new();
        let marking = Marking::new();
        assert_eq!(marking.token_count(&place_id), 0);
    }
}
