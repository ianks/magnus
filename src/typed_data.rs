//! Types and Traits for wrapping Rust types as Ruby objects.
//!
//! This, along with [`RTypedData`], provides a Rust API to the
//! `rb_data_typed_object_wrap` function from Ruby's C API.

use std::{
    collections::hash_map::DefaultHasher,
    ffi::{c_void, CString},
    fmt,
    hash::Hasher,
    marker::PhantomData,
    mem::size_of_val,
    ops::Deref,
    panic::catch_unwind,
    ptr,
};

#[cfg(ruby_gte_3_0)]
use rb_sys::rbimpl_typeddata_flags::{self, RUBY_TYPED_FREE_IMMEDIATELY, RUBY_TYPED_WB_PROTECTED};
use rb_sys::{self, rb_data_type_struct__bindgen_ty_1, rb_data_type_t, size_t, VALUE};

#[cfg(ruby_lt_3_0)]
const RUBY_TYPED_FREE_IMMEDIATELY: u32 = 1;

#[cfg(ruby_lt_3_0)]
const RUBY_TYPED_WB_PROTECTED: u32 = rb_sys::ruby_fl_type::RUBY_FL_WB_PROTECTED as u32;

use crate::{
    class::RClass,
    error::{bug_from_panic, Error},
    exception,
    into_value::IntoValue,
    object::Object,
    r_typed_data::RTypedData,
    ruby_handle::RubyHandle,
    try_convert::TryConvert,
    value::{private, ReprValue, Value},
};

/// A C struct containing metadata on a Rust type, for use with the
/// `rb_data_typed_object_wrap` API.
#[repr(transparent)]
pub struct DataType(rb_data_type_t);

impl DataType {
    /// Create a new `DataTypeBuilder`.
    ///
    /// `name` should be unique per wrapped type. It does not need to be a
    /// valid Ruby identifier.
    pub fn builder<T>(name: &'static str) -> DataTypeBuilder<T>
    where
        T: DataTypeFunctions,
    {
        DataTypeBuilder::new(name)
    }

    pub(crate) fn as_rb_data_type(&self) -> &rb_data_type_t {
        &self.0
    }
}

impl Drop for DataType {
    fn drop(&mut self) {
        unsafe {
            drop(CString::from_raw(self.0.wrap_struct_name as *mut _));
        }
    }
}

/// A helper trait used to define functions associated with a [`DataType`].
pub trait DataTypeFunctions
where
    Self: Send + Sized,
{
    /// Called when the Ruby wrapper object is garbage collected.
    ///
    /// This can be implemented to perform Ruby-specific clean up when your
    /// type is no longer referenced from Ruby, but it is likely easier to do
    /// this in a Drop implementation for your type.
    ///
    /// The default implementation simply drops `self`.
    ///
    /// This function **must not** panic. The process will abort if this
    /// function panics.
    fn free(self: Box<Self>) {}

    /// Called when Ruby marks this object as part of garbage collection.
    ///
    /// If your type contains any Ruby values you must mark each of those
    /// values in this function to avoid them being garbage collected.
    ///
    /// The default implementation does nothing.
    ///
    /// This function **must not** panic. The process will abort if this
    /// function panics.
    fn mark(&self) {}

    /// Called by Ruby to establish the memory size of this data, to optimise
    /// when garbage collection happens.
    ///
    /// The default implementation delegates to [`std::mem::size_of_val`].
    ///
    /// This function **must not** panic. The process will abort if this
    /// function panics.
    fn size(&self) -> usize {
        size_of_val(self)
    }

    /// Called during garbage collection.
    ///
    /// If your type contains any Ruby values that have been marked as moveable
    /// you must update them in this function.
    ///
    /// The default implementation does nothing.
    ///
    /// This function **must not** panic. The process will abort if this
    /// function panics.
    fn compact(&self) {}

    /// Extern wrapper for `free`. Don't define or call.
    ///
    /// # Safety
    ///
    /// `ptr` must be a vaild pointer to a `Box<Self>`, and must not be aliased
    /// This function will free the memory pointed to by `ptr`.
    ///
    /// This function must not panic.
    #[doc(hidden)]
    unsafe extern "C" fn extern_free(ptr: *mut c_void) {
        if let Err(e) = catch_unwind(|| Self::free(Box::from_raw(ptr as *mut _))) {
            bug_from_panic(e, "panic in DataTypeFunctions::free")
        }
    }

    /// Extern wrapper for `mark`. Don't define or call.
    ///
    /// # Safety
    ///
    /// `ptr` must be a vaild pointer to a `Self`, and must not be aliased.
    ///
    /// This function must not panic.
    #[doc(hidden)]
    unsafe extern "C" fn extern_mark(ptr: *mut c_void) {
        if let Err(e) = catch_unwind(|| Self::mark(&*(ptr as *mut Self))) {
            bug_from_panic(e, "panic in DataTypeFunctions::mark")
        }
    }

    /// Extern wrapper for `size`. Don't define or call.
    ///
    /// # Safety
    ///
    /// `ptr` must be a vaild pointer to a `Self`.
    ///
    /// This function must not panic.
    #[doc(hidden)]
    unsafe extern "C" fn extern_size(ptr: *const c_void) -> size_t {
        match catch_unwind(|| Self::size(&*(ptr as *const Self)) as size_t) {
            Ok(v) => v,
            Err(e) => bug_from_panic(e, "panic in DataTypeFunctions::size"),
        }
    }

    /// Extern wrapper for `compact`. Don't define or call.
    ///
    /// # Safety
    ///
    /// `ptr` must be a vaild pointer to a `Self`, and must not be aliased.
    ///
    /// This function must not panic.
    #[doc(hidden)]
    unsafe extern "C" fn extern_compact(ptr: *mut c_void) {
        if let Err(e) = catch_unwind(|| Self::compact(&*(ptr as *mut Self))) {
            bug_from_panic(e, "panic in DataTypeFunctions::compact")
        }
    }
}

/// A builder for [`DataType`].
pub struct DataTypeBuilder<T> {
    name: &'static str,
    mark: bool,
    size: bool,
    compact: bool,
    free_immediately: bool,
    wb_protected: bool,
    frozen_shareable: bool,
    phantom: PhantomData<T>,
}

impl<T> DataTypeBuilder<T>
where
    T: DataTypeFunctions,
{
    /// Create a new `DataTypeBuilder`.
    ///
    /// `name` should be unique per wrapped type. It does not need to be a
    /// valid Ruby identifier.
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            mark: false,
            size: false,
            compact: false,
            free_immediately: false,
            wb_protected: false,
            frozen_shareable: false,
            phantom: Default::default(),
        }
    }

    /// Enable using the the `mark` function from `<T as DataTypeFunctions>`.
    pub fn mark(&mut self) {
        self.mark = true;
    }

    /// Enable using the the `size` function from `<T as DataTypeFunctions>`.
    pub fn size(&mut self) {
        self.size = true;
    }

    /// Enable using the the `compact` function from `<T as DataTypeFunctions>`.
    pub fn compact(&mut self) {
        self.compact = true;
    }

    /// Enable the 'free_immediately' flag.
    ///
    /// This is safe to do as long as the `<T as DataTypeFunctions>::free`
    /// function or `T`'s drop function don't call Ruby in any way.
    ///
    /// If safe this should be enabled as this performs better and is more
    /// memory efficient.
    pub fn free_immediately(&mut self) {
        self.free_immediately = true;
    }

    /// Enable the 'write barrier protected' flag.
    ///
    /// You almost certainly don't want to enable this.
    pub fn wb_protected(&mut self) {
        self.wb_protected = true;
    }

    /// Enable the 'frozen_shareable' flag.
    ///
    /// Set this if your type is thread safe when the Ruby wrapper object is
    /// frozen.
    pub fn frozen_shareable(&mut self) {
        self.frozen_shareable = true;
    }

    /// Consume the builder and create a DataType.
    pub fn build(self) -> DataType {
        let mut flags = 0_usize as VALUE;
        if self.free_immediately {
            flags |= RUBY_TYPED_FREE_IMMEDIATELY as VALUE;
        }
        if self.wb_protected {
            flags |= RUBY_TYPED_WB_PROTECTED as VALUE;
        }
        #[cfg(ruby_gte_3_0)]
        if self.frozen_shareable {
            flags |= rbimpl_typeddata_flags::RUBY_TYPED_FROZEN_SHAREABLE as VALUE;
        }
        let dmark = self.mark.then(|| T::extern_mark as _);
        let dfree = Some(T::extern_free as _);
        let dsize = self.size.then(|| T::extern_size as _);
        #[cfg(ruby_gte_2_7)]
        let dcompact = self.compact.then(|| T::extern_compact as _);
        DataType(rb_data_type_t {
            wrap_struct_name: CString::new(self.name).unwrap().into_raw() as _,
            function: rb_data_type_struct__bindgen_ty_1 {
                dmark,
                dfree,
                dsize,
                #[cfg(ruby_gte_2_7)]
                dcompact,
                #[cfg(ruby_gte_2_7)]
                reserved: [ptr::null_mut(); 1],
                #[cfg(ruby_lt_2_7)]
                reserved: [ptr::null_mut(); 2],
            },
            parent: ptr::null(),
            data: ptr::null_mut(),
            flags,
        })
    }
}

/// A trait for Rust types that can be used with the `rb_data_typed_object_wrap`
/// API.
///
/// # Safety
///
/// This trait is unsafe to impliment as the fields of [`DataType`] returned by
/// [`TypedData::data_type`] control low level behaviour that can go very wrong
/// if set incorrectly. Implimenting this trait is the only way a [`DataType`]
/// can be passed to Ruby and result in safety violations, [`DataType`] is
/// otherwise safe (but useless) to create.
///
/// The [`TypedData`](`derive@crate::TypedData`) or [`wrap`](`crate::wrap`) macros can
/// help implementing this trait more safely.
pub unsafe trait TypedData
where
    Self: Send + Sized,
{
    /// Should return the class for the Ruby object wrapping the Rust type.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{define_class, memoize, RClass, Class};
    ///
    /// fn class() -> RClass {
    ///     *memoize!(RClass: {
    ///       let class = define_class("Foo", Default::default()).unwrap();
    ///       class.undef_alloc_func();
    ///       class
    ///     })
    /// }
    /// ```
    fn class() -> RClass;

    /// Should return a static reference to a [`DataType`] with metadata about
    /// the wrapped type.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{memoize, typed_data::DataTypeBuilder, DataType, DataTypeFunctions};
    ///
    /// #[derive(DataTypeFunctions)]
    /// struct Foo();
    ///
    /// fn data_type() -> &'static DataType {
    ///     memoize!(DataType: DataTypeBuilder::<Foo>::new("foo").build())
    /// }
    /// ```
    fn data_type() -> &'static DataType;
}

impl<T> TryConvert for &T
where
    T: TypedData,
{
    fn try_convert(val: Value) -> Result<Self, Error> {
        unsafe {
            RTypedData::from_value(val)
                .ok_or_else(|| {
                    Error::new(
                        exception::type_error(),
                        format!(
                            "no implicit conversion of {} into {}",
                            val.classname(),
                            T::class()
                        ),
                    )
                })?
                .get_unconstrained()
        }
    }
}

impl<T> IntoValue for T
where
    T: TypedData,
{
    fn into_value(self, handle: &RubyHandle) -> Value {
        handle.wrap(self).into()
    }
}

impl<T> From<T> for Value
where
    T: TypedData,
{
    fn from(data: T) -> Self {
        get_ruby!().into_value(data)
    }
}

/// A Ruby Object wrapping a Rust type `T`.
///
/// This is a Value pointer to a RTypedData struct, Ruby’s internal
/// representation of objects that wrap foreign types. Unlike [`RTypedData`] it
/// tracks the Rust type it should contains and errors early in [`TryConvert`]
/// if types don't match, rather than on [`Obj::get`].
///
/// All [`Value`] methods should be available on this type through [`Deref`],
/// but some may be missed by this documentation.
#[repr(transparent)]
pub struct Obj<T> {
    inner: RTypedData,
    phantom: PhantomData<T>,
}

impl<T> Copy for Obj<T> where T: TypedData {}

impl<T> Clone for Obj<T>
where
    T: TypedData,
{
    fn clone(&self) -> Self {
        *self
    }
}

impl RubyHandle {
    pub fn obj_wrap<T>(&self, data: T) -> Obj<T>
    where
        T: TypedData,
    {
        let inner = RTypedData::wrap(data);
        Obj {
            inner,
            phantom: PhantomData,
        }
    }
}

impl<T> Obj<T>
where
    T: TypedData,
{
    /// Wrap the Rust type `T` in a Ruby object.
    ///
    /// # Panics
    ///
    /// Panics if called from a non-Ruby thread.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{define_class, typed_data};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// #[magnus::wrap(class = "Point")]
    /// struct Point {
    ///     x: isize,
    ///     y: isize,
    /// }
    ///
    /// let point_class = define_class("Point", Default::default()).unwrap();
    ///
    /// let value = typed_data::Obj::wrap(Point { x: 4, y: 2 });
    /// assert!(value.is_kind_of(point_class));
    /// ```
    pub fn wrap(data: T) -> Self {
        get_ruby!().obj_wrap(data)
    }

    /// Get a reference to the Rust type wrapped in the Ruby object `self`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{define_class, typed_data};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// #[magnus::wrap(class = "Point")]
    /// #[derive(Debug, PartialEq, Eq)]
    /// struct Point {
    ///     x: isize,
    ///     y: isize,
    /// }
    ///
    /// let point_class = define_class("Point", Default::default()).unwrap();
    /// let value = typed_data::Obj::wrap(Point { x: 4, y: 2 });
    ///
    /// assert_eq!(value.get(), &Point { x: 4, y: 2 });
    /// ```
    pub fn get(&self) -> &T {
        self.inner.get().unwrap()
    }
}

impl<T> Deref for Obj<T>
where
    T: TypedData,
{
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        self.inner.deref()
    }
}

impl<T> fmt::Display for Obj<T>
where
    T: TypedData,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl<T> fmt::Debug for Obj<T>
where
    T: TypedData,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inspect())
    }
}

impl<T> IntoValue for Obj<T>
where
    T: TypedData,
{
    fn into_value(self, _: &RubyHandle) -> Value {
        *self.inner
    }
}

impl<T> From<Obj<T>> for Value
where
    T: TypedData,
{
    fn from(val: Obj<T>) -> Self {
        *val.inner
    }
}

impl<T> From<Obj<T>> for RTypedData
where
    T: TypedData,
{
    fn from(val: Obj<T>) -> Self {
        val.inner
    }
}

impl<T> Object for Obj<T> where T: TypedData {}

unsafe impl<T> private::ReprValue for Obj<T>
where
    T: TypedData,
{
    fn to_value(self) -> Value {
        *self.inner
    }

    unsafe fn from_value_unchecked(val: Value) -> Self {
        Self {
            inner: RTypedData::from_value_unchecked(val),
            phantom: PhantomData,
        }
    }
}

impl<T> ReprValue for Obj<T> where T: TypedData {}

impl<T> TryConvert for Obj<T>
where
    T: TypedData,
{
    fn try_convert(val: Value) -> Result<Self, Error> {
        let inner = RTypedData::from_value(val).ok_or_else(|| {
            Error::new(
                exception::type_error(),
                format!(
                    "no implicit conversion of {} into {}",
                    unsafe { val.classname() },
                    T::class()
                ),
            )
        })?;

        // check it really does contain a T
        inner.get::<T>()?;

        Ok(Self {
            inner,
            phantom: PhantomData,
        })
    }
}

/// Trait for a Ruby-compatible `#hash` method.
///
/// Automatically implemented for any type implementing [`std::hash::Hash`].
///
/// See also [`IsEql`].
///
/// # Examples
///
/// ```
/// use std::hash::Hasher;
///
/// use magnus::{
///     define_class, embed::init, function, gc, method, typed_data, DataTypeFunctions, Error, Module,
///     Object, RHash, TypedData, Value,
/// };
///
/// #[derive(TypedData)]
/// #[magnus(class = "Pair", free_immediatly, mark)]
/// struct Pair {
///     a: Value,
///     b: Value,
/// }
///
/// impl Pair {
///     fn new(a: Value, b: Value) -> Self {
///         Self { a, b }
///     }
/// }
///
/// impl DataTypeFunctions for Pair {
///     fn mark(&self) {
///         gc::mark(&self.a);
///         gc::mark(&self.b);
///     }
/// }
///
/// impl std::hash::Hash for Pair {
///     fn hash<H: Hasher>(&self, state: &mut H) {
///         state.write_i64(
///             self.a
///                 .hash()
///                 .expect("#hash should not fail")
///                 .to_i64()
///                 .expect("#hash result guaranteed to be <= i64"),
///         );
///         state.write_i64(
///             self.b
///                 .hash()
///                 .expect("#hash should not fail")
///                 .to_i64()
///                 .expect("#hash result guaranteed to be <= i64"),
///         );
///     }
/// }
///
/// impl PartialEq for Pair {
///     fn eq(&self, other: &Self) -> bool {
///         self.a.eql(&other.a).unwrap_or(false) && self.b.eql(&other.b).unwrap_or(false)
///     }
/// }
///
/// impl Eq for Pair {}
///
/// let _cleanup = unsafe { init() };
///
/// let class = define_class("Pair", Default::default()).unwrap();
/// class
///     .define_singleton_method("new", function!(Pair::new, 2))
///     .unwrap();
/// class
///     .define_method("hash", method!(<Pair as typed_data::Hash>::hash, 0))
///     .unwrap();
/// class
///     .define_method("eql?", method!(<Pair as typed_data::IsEql>::is_eql, 1))
///     .unwrap();
///
/// let a = Pair::new(Value::from("foo"), Value::from(1));
/// let hash = RHash::new();
/// hash.aset(a, "test value").unwrap();
///
/// let b = Pair::new(Value::from("foo"), Value::from(1));
/// assert_eq!("test value", hash.fetch::<_, String>(b).unwrap());
///
/// let c = Pair::new(Value::from("bar"), Value::from(2));
/// assert!(hash.get(c).is_none());
/// ```
pub trait Hash {
    // Docs at trait level.
    #![allow(missing_docs)]
    fn hash(&self) -> i64;
}

impl<T> Hash for T
where
    T: std::hash::Hash,
{
    fn hash(&self) -> i64 {
        let mut hasher = DefaultHasher::new();
        std::hash::Hash::hash(self, &mut hasher);
        // Ensure the Rust usize hash converts nicely to Ruby's expected range
        // if we return usize it'd truncate to 0 for anything negative.
        hasher.finish() as i64
    }
}

/// Trait for a Ruby-compatible `#eql?` method.
///
/// Automatically implemented for any type implementing [`Eq`] and
/// [`TryConvert`].
///
/// See also [`typed_data::Hash`](Hash).
///
/// # Examples
///
/// ```
/// use std::hash::Hasher;
///
/// use magnus::{
///     define_class, embed::init, function, gc, method, typed_data, DataTypeFunctions, Error, Module,
///     Object, RHash, TypedData, Value,
/// };
///
/// #[derive(TypedData)]
/// #[magnus(class = "Pair", free_immediatly, mark)]
/// struct Pair {
///     a: Value,
///     b: Value,
/// }
///
/// impl Pair {
///     fn new(a: Value, b: Value) -> Self {
///         Self { a, b }
///     }
/// }
///
/// impl DataTypeFunctions for Pair {
///     fn mark(&self) {
///         gc::mark(&self.a);
///         gc::mark(&self.b);
///     }
/// }
///
/// impl std::hash::Hash for Pair {
///     fn hash<H: Hasher>(&self, state: &mut H) {
///         state.write_i64(
///             self.a
///                 .hash()
///                 .expect("#hash should not fail")
///                 .to_i64()
///                 .expect("#hash result guaranteed to be <= i64"),
///         );
///         state.write_i64(
///             self.b
///                 .hash()
///                 .expect("#hash should not fail")
///                 .to_i64()
///                 .expect("#hash result guaranteed to be <= i64"),
///         );
///     }
/// }
///
/// impl PartialEq for Pair {
///     fn eq(&self, other: &Self) -> bool {
///         self.a.eql(&other.a).unwrap_or(false) && self.b.eql(&other.b).unwrap_or(false)
///     }
/// }
///
/// impl Eq for Pair {}
///
/// let _cleanup = unsafe { init() };
///
/// let class = define_class("Pair", Default::default()).unwrap();
/// class
///     .define_singleton_method("new", function!(Pair::new, 2))
///     .unwrap();
/// class
///     .define_method("hash", method!(<Pair as typed_data::Hash>::hash, 0))
///     .unwrap();
/// class
///     .define_method("eql?", method!(<Pair as typed_data::IsEql>::is_eql, 1))
///     .unwrap();
///
/// let a = Pair::new(Value::from("foo"), Value::from(1));
/// let hash = RHash::new();
/// hash.aset(a, "test value").unwrap();
///
/// let b = Pair::new(Value::from("foo"), Value::from(1));
/// assert_eq!("test value", hash.fetch::<_, String>(b).unwrap());
///
/// let c = Pair::new(Value::from("bar"), Value::from(2));
/// assert!(hash.get(c).is_none());
/// ```
pub trait IsEql {
    // Docs at trait level.
    #![allow(missing_docs)]
    fn is_eql(&self, other: Value) -> bool;
}

impl<'a, T> IsEql for T
where
    T: Eq + 'a,
    &'a T: TryConvert,
{
    fn is_eql(&self, other: Value) -> bool {
        other
            .try_convert::<&'a T>()
            .map(|o| self == o)
            .unwrap_or(false)
    }
}
