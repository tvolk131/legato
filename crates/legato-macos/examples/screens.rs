fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("{:#?}", legato_macos::screens());
        println!("cursor: {:?}", legato_macos::cursor_position());
        println!("permissions: {:?}", legato_macos::Permissions::check());
        println!("{:?}", legato_macos::receiver_config());
    }
}
