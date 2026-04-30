trait Display {}

trait LocalTrait {}

struct Local;

struct Generic<T> {
    value: T,
}

impl crate::Local {
    fn new() -> Self {
        Self
    }
}

impl<T> Generic<T> {
    fn value(&self) -> &T {
        &self.value
    }
}

impl<T> Generic<T> {
    fn into_value(self) -> T {
        self.value
    }
}

impl<T> LocalTrait for Generic<T> {}

impl<T> Display for Generic<T> {}

impl<T> std::fmt::Display for Generic<T>
where
    T: std::fmt::Display,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(formatter)
    }
}

impl Default for Generic<u8> {
    fn default() -> Self {
        Self { value: 0 }
    }
}

impl external::Local {
    fn external() {}
}
