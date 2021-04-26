//! # Overview
//!
//! `self_cell` provides one macro-rules macro: [`self_cell`]. With this macro you
//! can create self-referential structs that are safe-to-use in stable Rust,
//! without leaking the struct internal lifetime.
//!
//! In a nutshell, the API looks *roughly* like this:
//!
//! ```ignore
//! // User code:
//!
//! self_cell!(
//!     struct NewStructName {
//!         #[from]
//!         owner: Owner,
//!
//!         #[covariant]
//!         dependent: Dependent,
//!     }
//!     
//!     impl {Debug}
//! );
//!
//! // Generated by macro:
//!
//! struct NewStructName(...);
//!
//! impl NewStructName {
//!     fn new(owner: Owner) -> NewStructName { ... }
//!     fn borrow_owner<'a>(&'a self) -> &'a Owner { ... }
//!     fn borrow_dependent<'a>(&'a self) -> &'a Dependent<'a> { ... }
//! }
//!
//! impl Debug for NewStructName { ... }
//! ```
//!
//! Self-referential structs are currently not supported with safe vanilla Rust.
//! The only reasonable safe alternative is to expect the user to juggle 2
//! separate data structures which is a mess. The library solution ouroboros is
//! really expensive to compile due to its use of procedural macros.
//!
//! This alternative is `no_std`, uses no proc-macros, some self contained
//! unsafe and works on stable Rust, and is miri tested. With a total of less
//! than 300 lines of implementation code, which consists mostly of type and
//! trait implementations, this crate aims to be a good minimal solution to the
//! problem of self-referential structs.
//!
//! It has undergone [community code
//! review](https://users.rust-lang.org/t/experimental-safe-to-use-proc-macro-free-self-referential-structs-in-stable-rust/52775)
//! from experienced Rust users.
//!
//! ### Fast compile times
//!
//! ```ignore
//! $ rm -rf target && cargo +nightly build -Z timings
//!
//! Compiling self_cell v0.7.0
//! Completed self_cell v0.7.0 in 0.2s
//! ```
//!
//! Because it does **not** use proc-macros, and has 0 dependencies
//! compile-times are fast.
//!
//! Measurements done on a slow laptop.
//!
//! ### A motivating use case
//!
//! ```rust
//! use self_cell::self_cell;
//!
//! #[derive(Debug, Eq, PartialEq)]
//! struct Ast<'a>(pub Vec<&'a str>);
//!
//! impl<'a> From<&'a String> for Ast<'a> {
//!     fn from(code: &'a String) -> Self {
//!         // Placeholder for expensive parsing.
//!         Ast(code.split(' ').filter(|word| word.len() > 1).collect())
//!     }
//! }
//!
//! self_cell!(
//!     struct AstCell {
//!         #[from]
//!         owner: String,
//!
//!         #[covariant]
//!         dependent: Ast,
//!     }
//!
//!     impl {Clone, Debug, Eq, PartialEq}
//! );
//!
//! fn build_ast_cell(code: &str) -> AstCell {
//!     // Create owning String on stack.
//!     let pre_processed_code = code.trim().to_string();
//!
//!     // Move String into AstCell, build Ast by calling pre_processed_code.into()
//!     // and then return the AstCell.
//!     AstCell::new(pre_processed_code)
//! }
//!
//! fn main() {
//!     let ast_cell = build_ast_cell("fox = cat + dog");
//!     dbg!(&ast_cell);
//!     dbg!(ast_cell.borrow_owner());
//!     dbg!(ast_cell.borrow_dependent().0[1]);
//! }
//! ```
//!
//! ```txt
//! $ cargo run
//!
//! [src/main.rs:33] &ast_cell = AstCell { owner: "fox = cat + dog", dependent: Ast(["fox", "cat", "dog"]) }
//! [src/main.rs:34] ast_cell.borrow_owner() = "fox = cat + dog"
//! [src/main.rs:35] ast_cell.borrow_dependent().0[1] = "cat"
//! ```
//!
//! There is no way in safe Rust to have an API like `build_ast_cell`, as soon
//! as `Ast` depends on stack variables like `pre_processed_code` you can't
//! return the value out of the function anymore. You could move the
//! pre-processing into the caller but that gets ugly quickly because you can't
//! encapsulate things anymore. Note this is a somewhat niche use case,
//! self-referential structs should only be used when there is no good
//! alternative.
//!
//! Under the hood, it heap allocates a struct which it initializes first by
//! moving the owner value to it and then using the reference to this now
//! Pin/Immovable owner to construct the dependent inplace next to it. This
//! makes it safe to move the generated SelfCell but you have to pay for the
//! heap allocation.
//!
//! See the documentation for [`self_cell`] to dive further into the details.
//!
//! Or take a look at the advanced examples:
//! - [Example how to handle dependent construction that can fail](https://github.com/Voultapher/once_self_cell/tree/main/examples/fallible_dependent_construction)
//!
//! - [How to build a lazy AST with self_cell](https://github.com/Voultapher/once_self_cell/tree/main/examples/lazy_ast)
//!
//! - [How to avoid leaking memory if `Dependen::from(&Owner)` panics](https://github.com/Voultapher/once_self_cell/tree/main/examples/no_leak_panic)

#![no_std]

#[doc(hidden)]
pub extern crate alloc;

#[doc(hidden)]
pub mod unsafe_self_cell;

#[doc(hidden)]
#[macro_export]
macro_rules! _cell_constructor {
    (from, $Vis:vis, $Owner:ty, $Dependent:ident) => {
        $Vis fn new(owner: $Owner) -> Self {
            unsafe {
                // All this has to happen here, because there is not good way
                // of passing the appropriate logic into UnsafeSelfCell::new
                // short of assuming Dependent<'static> is the same as
                // Dependent<'a>, which I'm not confident is safe.

                type JoinedCell<'a> = $crate::unsafe_self_cell::JoinedCell<$Owner, $Dependent<'a>>;

                let layout = $crate::alloc::alloc::Layout::new::<JoinedCell>();

                let joined_void_ptr = $crate::alloc::alloc::alloc(layout);

                let joined_ptr = core::mem::transmute::<*mut u8, *mut JoinedCell>(joined_void_ptr);

                // Move owner into newly allocated space.
                core::ptr::addr_of_mut!((*joined_ptr).owner).write(owner);

                // Initialize dependent with owner reference in final place.
                core::ptr::addr_of_mut!((*joined_ptr).dependent)
                    .write(core::convert::Into::into((&(*joined_ptr).owner)));

                Self {
                    unsafe_self_cell: $crate::unsafe_self_cell::UnsafeSelfCell::new(
                        joined_void_ptr,
                    ),
                }
            }
        }
    };
    (try_from, $Vis:vis, $Owner:ty, $Dependent:ident) => {
        $Vis fn try_from<'a>(
            owner: $Owner,
        ) -> Result<Self, <&'a $Owner as core::convert::TryInto<$Dependent<'a>>>::Error> {
            unsafe {
                // All this has to happen here, because there is not good way
                // of passing the appropriate logic into UnsafeSelfCell::new
                // short of assuming Dependent<'static> is the same as
                // Dependent<'a>, which I'm not confident is safe.

                type JoinedCell<'a> = $crate::unsafe_self_cell::JoinedCell<$Owner, $Dependent<'a>>;

                let layout = $crate::alloc::alloc::Layout::new::<JoinedCell>();

                let joined_void_ptr = $crate::alloc::alloc::alloc(layout);

                let joined_ptr = core::mem::transmute::<*mut u8, *mut JoinedCell>(joined_void_ptr);

                // Move owner into newly allocated space.
                core::ptr::addr_of_mut!((*joined_ptr).owner).write(owner);

                type Error<'a> = <&'a $Owner as core::convert::TryInto<$Dependent<'a>>>::Error;

                // Attempt to initialize dependent with owner reference in final place.
                let try_inplace_init = || -> Result<(), Error<'a>> {
                    core::ptr::addr_of_mut!((*joined_ptr).dependent)
                        .write(core::convert::TryInto::try_into(&(*joined_ptr).owner)?);

                    Ok(())
                };

                match try_inplace_init() {
                    Ok(()) => Ok(Self {
                        unsafe_self_cell: $crate::unsafe_self_cell::UnsafeSelfCell::new(
                            joined_void_ptr,
                        ),
                    }),
                    Err(err) => {
                        // Clean up partially initialized joined_cell.
                        core::ptr::drop_in_place(core::ptr::addr_of_mut!((*joined_ptr).owner));

                        $crate::alloc::alloc::dealloc(joined_void_ptr, layout);

                        Err(err)
                    }
                }
            }
        }
    };
    (from_fn, $Vis:vis, $Owner:ty, $Dependent:ident) => {
        $Vis fn from_fn(
            owner: $Owner,
            dependent_builder: impl for<'a> FnOnce(&'a $Owner) -> $Dependent<'a>
        ) -> Self {
            unsafe {
                // All this has to happen here, because there is not good way
                // of passing the appropriate logic into UnsafeSelfCell::new
                // short of assuming Dependent<'static> is the same as
                // Dependent<'a>, which I'm not confident is safe.

                // For this API to be safe there has to be no safe way to
                // capture additional references in `dependent_builder` and then
                // return them as part of Dependent. Eg. it should be impossible
                // to express: 'a should outlive 'x here `fn
                // bad<'a>(outside_ref: &'a String) -> impl for<'x> FnOnce(&'x
                // Owner) -> Dependent<'x>`.

                // Also because we don't want to store the FnOnce, using this
                // ctor means Clone can't be automatically implemented.

                type JoinedCell<'a> = $crate::unsafe_self_cell::JoinedCell<$Owner, $Dependent<'a>>;

                let layout = $crate::alloc::alloc::Layout::new::<JoinedCell>();

                let joined_void_ptr = $crate::alloc::alloc::alloc(layout);

                let joined_ptr = core::mem::transmute::<*mut u8, *mut JoinedCell>(joined_void_ptr);

                // Move owner into newly allocated space.
                core::ptr::addr_of_mut!((*joined_ptr).owner).write(owner);

                // Initialize dependent with owner reference in final place.
                core::ptr::addr_of_mut!((*joined_ptr).dependent)
                    .write(dependent_builder((&(*joined_ptr).owner)));

                Self {
                    unsafe_self_cell: $crate::unsafe_self_cell::UnsafeSelfCell::new(
                        joined_void_ptr,
                    ),
                }
            }
        }
    };
    ($x:ident, $Vis:vis, $Owner:ty, $Dependent:ident) => {
        compile_error!("This macro only accepts `from`, `try_from` or `from_fn`");
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! _covariant_access {
    (covariant, $Vis:vis, $Dependent:ident) => {
        $Vis fn borrow_dependent<'a>(&'a self) -> &'a $Dependent<'a> {
            fn _assert_covariance<'x: 'y, 'y>(x: $Dependent<'x>) -> $Dependent<'y> {
                //  This function only compiles for covariant types.
                x // Change the macro invocation to not_covariant.
            }

            unsafe { self.unsafe_self_cell.borrow_dependent() }
        }
    };
    (not_covariant, $Vis:vis, $Dependent:ident) => {
        // For types that are not covariant it's unsafe to allow
        // returning direct references.
        // For example a lifetime that is too short could be chosen:
        // See https://github.com/Voultapher/self_cell/issues/5
    };
    ($x:ident, $Vis:vis, $Dependent:ident) => {
        compile_error!("This macro only accepts `covariant` or `not_covariant`");
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! _impl_automatic_derive {
    (Clone, $StructName:ident) => {
        impl Clone for $StructName {
            fn clone(&self) -> Self {
                // TODO support try_from.
                Self::new(self.borrow_owner().clone())
            }
        }
    };
    (Debug, $StructName:ident) => {
        impl core::fmt::Debug for $StructName {
            fn fmt(&self, fmt: &mut core::fmt::Formatter) -> Result<(), core::fmt::Error> {
                self.with_dependent(|owner, dependent| {
                    write!(
                        fmt,
                        concat!(
                            stringify!($StructName),
                            " {{ owner: {:?}, dependent: {:?} }}"
                        ),
                        owner, dependent
                    )
                })
            }
        }
    };
    (PartialEq, $StructName:ident) => {
        impl PartialEq for $StructName {
            fn eq(&self, other: &Self) -> bool {
                *self.borrow_owner() == *other.borrow_owner()
            }
        }
    };
    (Eq, $StructName:ident) => {
        // TODO this should only be allowed if owner is Eq.
        impl Eq for $StructName {}
    };
    (Hash, $StructName:ident) => {
        impl core::hash::Hash for $StructName {
            fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
                self.borrow_owner().hash(state);
            }
        }
    };
    ($x:ident, $StructName:ident) => {
        compile_error!(concat!(
            "No automatic trait impl for trait: ",
            stringify!($x)
        ));
    };
}

/// This macro declares a new struct of `$StructName` and implements traits
/// based on `$AutomaticDerive`.
///
/// Example:
/// ```rust
/// use self_cell::self_cell;
///
/// #[derive(Debug, Eq, PartialEq)]
/// struct Ast<'a>(pub Vec<&'a str>);
///
/// impl<'a> From<&'a String> for Ast<'a> {
///     fn from(body: &'a String) -> Self {
///         Ast(vec![&body[2..5], &body[1..3]])
///     }
/// }
///
/// self_cell!(
///     #[doc(hidden)]
///     struct PackedAstCell {
///         #[from]
///         owner: String,
///
///         #[covariant]
///         dependent: Ast,
///     }
///
///     impl {Clone, Debug, PartialEq, Eq, Hash}
/// );
/// ```
///
/// See the crate overview to get a get an overview and a motivating example.
///
/// ### Parameters:
///
/// - `$Vis:vis struct $StructName:ident` Name of the struct that will be
///   declared, this needs to be unique for the relevant scope. Example: `struct
///   AstCell` or `pub struct AstCell`.
///
///   `$(#[$StructMeta:meta])*` allows you specify further meta items for this
///   struct, eg. `#[doc(hidden)] struct AstCell`.
///
/// - `$ConstructorType:ident` Marker declaring if a regular `::new` or
///   `::try_from` constructor should be generated. Possible Values:
///   * **from**: This generates a `fn new(owner: $Owner) -> Self` constructor.
///     For this to work `<&'a $Owner>::Into<$Dependent<'a>>` has to be
///     implemented.
///
///   * **try_from**: This generates a `fn try_from<'a>(owner: $Owner) ->
///     Result<Self, <&'a $Owner as TryInto<$Dependent<'a>>>::Error>`
///     constructor, which allows fallible construction without having to check
///     for failure every time dependent is accessed. For this to work `<&'a
///     $Owner>::TryInto<$Dependent<'a>>` has to be implemented.
///
///   * **from_fn**: This generates a `fn from_fn(owner: $Owner,
///     dependent_builder: impl for<'a> FnOnce(&'a $Owner) -> $Dependent<'a>) ->
///     Self` constructor, which allows more flexible construction that can also
///     return additional unrelated state. But has the drawback of preventing
///     Clone from being automatically implemented. A Fn or FnMut would have to
///     be stored to enable this. However you can still implement Clone
///     yourself.
///
///   NOTE: If `<&'a $Owner>::Into<$Dependent<'a>>` panics, the value of owner
///   and a heap struct will be leaked. This is safe, but might not be what you
///   want. See [How to avoid leaking memory if `Dependen::from(&Owner)`
///   panics](https://github.com/Voultapher/once_self_cell/tree/main/examples/no_leak_panic).
///
/// - `$Owner:ty` Type of owner. This has to have a `'static` lifetime. Example:
///   `String`.
///
/// - `$Dependent:ident` Name of the dependent type without specified lifetime.
///   This can't be a nested type name. As workaround either create a type alias
///   `type Dep<'a> = Option<Vec<&'a str>>;` or create a new-type `struct
///   Dep<'a>(Option<Vec<&'a str>>);`. Example: `Ast`.
///
///   `$Covariance:ident` Marker declaring if `$Dependent` is
///   [covariant](https://doc.rust-lang.org/nightly/nomicon/subtyping.html).
///   Possible Values:
///
///   * **covariant**: This generates the direct reference accessor function `fn
///     borrow_dependent<'a>(&'a self) -> &'a $Dependent<'a>`. This is only safe
///     to do if this compiles `fn _assert_covariance<'x: 'y, 'y>(x:
///     $Dependent<'x>) -> $Dependent<'y> { x }`. Otherwise you could choose a
///     lifetime that is too short for types with interior mutability like
///     `Cell`, which can lead to UB in safe code. Which would violate the
///     promise of this library that it is safe-to-use. If you accidentally mark
///     a type that is not covariant as covariant, you will get a compile time
///     error.
///
///   * **not_covariant**: This generates no additional code but you can use `fn
///     with_dependent<Ret>(&self, func: impl for<'a> FnOnce(&'a $Owner, &'a
///     $Dependent<'a>) -> Ret) -> Ret`. See [How to build a lazy AST with
///     self_cell](https://github.com/Voultapher/once_self_cell/tree/main/examples/lazy_ast)
///     for a usage example.
///
/// - `impl {$($AutomaticDerive:ident),*},` Optional comma separated list of
///   optional automatic trait implementations. Possible Values:
///   * **Clone**: Logic `cloned_owner = owner.clone()` and then calls
///     `cloned_owner.into()` to create cloned SelfCell.
///
///   * **Debug**: Prints the debug representation of owner and dependent.
///     Example: `AstCell { owner: "fox = cat + dog", dependent: Ast(["fox",
///     "cat", "dog"]) }`
///
///   * **PartialEq**: Logic `*self.borrow_owner() == *other.borrow_owner()`,
///     this assumes that `Dependent<'a>::From<&'a Owner>` is deterministic, so
///     that only comparing owner is enough.
///
///   * **Eq**: Will implement the trait marker `Eq` for `$StructName`. Beware
///     if you select this `Eq` will be implemented regardless if `$Owner`
///     implements `Eq`, that's an unfortunate technical limitation.
///
///   * **Hash**: Logic `self.borrow_owner().hash(state);`, this assumes that
///     `Dependent<'a>::From<&'a Owner>` is deterministic, so that only hashing
///     owner is enough.
///
///   All `AutomaticDerive` are optional and you can implement you own version
///   of these traits. The declared struct is part of your module and you are
///   free to implement any trait in any way you want. Access to the unsafe
///   internals is only possible via unsafe functions, so you can't accidentally
///   use them in safe code.
///
#[macro_export]
macro_rules! self_cell {
    (
        $(#[$StructMeta:meta])*
        $Vis:vis struct $StructName:ident {
            #[$ConstructorType:ident]
            owner: $Owner:ty,

            #[$Covariance:ident]
            dependent: $Dependent:ident,
        }

        $(impl {$($AutomaticDerive:ident),*})?
    ) => {
        $(#[$StructMeta])*
        $Vis struct $StructName {
            unsafe_self_cell: $crate::unsafe_self_cell::UnsafeSelfCell<
                $Owner,
                $Dependent<'static>
            >
        }

        impl $StructName {
            $crate::_cell_constructor!($ConstructorType, $Vis, $Owner, $Dependent);

            $Vis fn borrow_owner<'a>(&'a self) -> &'a $Owner {
                unsafe { self.unsafe_self_cell.borrow_owner::<$Dependent<'a>>() }
            }

            $Vis fn with_dependent<Ret>(&self, func: impl for<'a> FnOnce(&'a $Owner, &'a $Dependent<'a>) -> Ret) -> Ret {
                unsafe {
                    func(
                        self.unsafe_self_cell.borrow_owner::<$Dependent>(),
                        self.unsafe_self_cell.borrow_dependent()
                    )
                }
            }

            $crate::_covariant_access!($Covariance, $Vis, $Dependent);
        }

        impl Drop for $StructName {
            fn drop<'a>(&mut self) {
                unsafe {
                    self.unsafe_self_cell.drop_joined::<$Dependent>();
                }
            }
        }

        // The user has to choose which traits can and should be automatically
        // implemented for the cell.
        $($(
            $crate::_impl_automatic_derive!($AutomaticDerive, $StructName);
        )*)*
    };
}
