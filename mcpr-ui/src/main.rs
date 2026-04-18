use yew::prelude::*;

#[function_component]
fn App() -> Html {
    html! {
        <div class="min-h-screen bg-base-200 flex items-center justify-center">
            <div class="card bg-base-100 shadow-xl">
                <div class="card-body items-center text-center">
                    <h1 class="card-title text-3xl">{ "mcpr-ui" }</h1>
                    <p class="py-2">{ "Yew + Tailwind CSS + daisyUI" }</p>
                    <div class="card-actions">
                        <button class="btn btn-primary">{ "Hello daisyUI" }</button>
                    </div>
                </div>
            </div>
        </div>
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
