mod app;
mod export;
mod merge;

fn main() {
    yew::Renderer::<app::App>::new().render();
}
