fn main() {
    println!("hello. this is unused");
}

#[cfg(test)]
mod test {
    #[test]
    #[should_panic]
    fn hecs_repeated_component() {
        use hecs::*;

        let mut world = World::new();
        let e = world.spawn((0.0f32, 1.0f32, 2.0f32));
        assert_eq!(2.0f32, *world.get::<&f32>(e).unwrap());
    }

    #[test]
    #[should_panic]
    fn legion_repeated_component() {
        use legion::*;

        let mut world = World::default();
        let e = world.push((0.0f32, 1.0f32, 2.0f32));
        let entry = world.entry(e).unwrap();
        assert_eq!(2.0f32, *entry.get_component::<f32>().unwrap());
    }
}
