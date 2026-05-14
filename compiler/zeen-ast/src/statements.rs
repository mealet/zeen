#[derive(Debug)]
pub enum Statements<'arena> {
    Foo(&'arena usize),
}
