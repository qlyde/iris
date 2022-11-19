pub trait Handler<T> {
    type Result;

    fn handle(&mut self, message: T) -> Self::Result;
}
