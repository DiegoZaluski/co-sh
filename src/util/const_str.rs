//! Compile-time string-constant composition.
//!
//! `concat!` accepts string LITERALS only — it cannot compose existing
//! `const &str` paths (a const path in a `concat!` argument is a compile
//! error). Where a composite constant must be built from parts that ALSO
//! exist as standalone constants (each part usable alone), this macro
//! defines every part from its own literal list AND the joined constant
//! from the concatenation of all parts — the literals are written exactly
//! once and can never drift apart between the part and the whole.

/// Defines one or more `pub const &str` parts, plus a final constant equal
/// to their byte-exact concatenation, in order.
///
/// Each part's body is a comma-separated token list that `concat!`
/// accepts: plain string literals (the classic form) or zero-argument
/// macro invocations that each expand to a single `concat!(...)` — the
/// latter lets shared literal groups feed several composites while being
/// written exactly once.
///
/// ```ignore
/// cosh::const_str_joined! {
///     /// The full text.
///     FULL : joined [
///         /// First part, usable alone.
///         PART_A = [
///             "line one\n",
///             "line two\n",
///         ]
///         /// Second part, usable alone.
///         PART_B = [
///             "line three\n",
///         ]
///     ]
/// }
/// ```
///
/// A part may alternatively be a zero-argument macro invocation whose
/// body is a single `concat!(...)` (`PART = shared_text!()`), letting one
/// literal group feed several composites. The invoked macro must be in
/// scope at the expansion site (textual scoping — define it before use in
/// the same module or import it at the crate root).
///
/// Every output is `const`-evaluated (no runtime cost, stays `&'static
/// str`); `FULL` is byte-identical to `PART_A` immediately followed by
/// `PART_B`.
#[macro_export]
macro_rules! const_str_joined {
    (
        $(#[$full_doc:meta])*
        $full:ident : joined [
            $(
                $(#[$part_doc:meta])*
                $part:ident = [ $($lit:tt),* $(,)? ]
            )+
        ] $(,)?
    ) => {
        $(
            $(#[$part_doc])*
            pub const $part: &str = concat!($($lit),*);
        )+
        $(#[$full_doc])*
        pub const $full: &str = concat!( $( $($lit),* , )+ );
    };
    // Alternative part form: a zero-argument macro invocation whose body
    // is itself a `concat!(...)`. This lets one shared literal group feed
    // several composites (the part constant, composites containing it,
    // and composites that extend it) while the literals are written
    // exactly once — `concat!` itself accepts such an invocation as an
    // argument, so the full constant concatenates the same expansions.
    (
        $(#[$full_doc:meta])*
        $full:ident : joined [
            $(
                $(#[$part_doc:meta])*
                $part:ident = $mac:ident ! ( )
            ),+ $(,)?
        ] $(,)?
    ) => {
        $(
            $(#[$part_doc])*
            pub const $part: &str = $mac ! ();
        )+
        $(#[$full_doc])*
        pub const $full: &str = concat!( $( $mac ! () , )+ );
    };
}

#[cfg(test)]
mod tests {
    crate::const_str_joined! {
        /// Test composite.
        TEST_FULL : joined [
            /// First part.
            TEST_A = [
                "alpha\n",
                "beta\n",
            ]
            /// Second part.
            TEST_B = [
                "gamma\n",
            ]
        ]
    }

    #[test]
    fn parts_and_join_agree_byte_for_byte() {
        assert_eq!(TEST_FULL, "alpha\nbeta\ngamma\n");
        assert_eq!(TEST_A, "alpha\nbeta\n");
        assert_eq!(TEST_B, "gamma\n");
        assert!(TEST_FULL.starts_with(TEST_A));
        assert_eq!(&TEST_FULL[TEST_A.len()..], TEST_B);
    }
}
