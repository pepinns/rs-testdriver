#![allow(unused_imports, unused_variables, dead_code)]
#![feature(proc_macro_hygiene, type_alias_impl_trait)]
#![feature(decl_macro)]
#![feature(exit_status_error)]

pub mod driver;
pub mod error;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
