use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::System::LibraryLoader::*;
use windows::core::*;
use windows::core::HSTRING;
use serde::{Deserialize, Serialize};
use base64::{Engine as _, engine::general_purpose};
use image::ImageFormat;
use reqwest::Client;
use lazy_static::lazy_static;
use anyhow::Result;
use std::io::{self, Write};

#[derive(Serialize, Deserialize)]
struct GroqResponse {
    translation: String,
    r#box: BoxCoords,
}

#[derive(Serialize, Deserialize)]
struct BoxCoords {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Serialize, Deserialize, Clone)]
struct Config {
    api_key: String,
    language: String,
    hotkey: u32,
}

struct App {
    config: Config,
    runtime: Runtime,
    client: Client,
}

fn load_config() -> Config {
    let config_dir = dirs::config_dir().unwrap().join("screen-grounded-translator");
    let _ = std::fs::create_dir_all(&config_dir);
    let config_file = config_dir.join("config.json");
    if config_file.exists() {
        let data = std::fs::read_to_string(config_file).unwrap_or_default();
        serde_json::from_str(&data).unwrap_or_else(|_| Config {
            api_key: "".to_string(),
            language: "Vietnamese".to_string(),
            hotkey: VK_OEM_3.0 as u32,
        })
    } else {
        Config {
            api_key: "".to_string(),
            language: "Vietnamese".to_string(),
            hotkey: VK_OEM_3.0 as u32,
        }
    }
}

fn save_config(config: &Config) {
    let config_dir = dirs::config_dir().unwrap().join("screen-grounded-translator");
    let _ = std::fs::create_dir_all(&config_dir);
    let config_file = config_dir.join("config.json");
    let data = serde_json::to_string(config).unwrap();
    let _ = std::fs::write(config_file, data);
}

fn run_config_prompt() {
    let config = load_config();

    println!("\n=== Screen Translator Configuration ===\n");

    // API Key
    println!("Current API Key: {}", if config.api_key.is_empty() { "[empty]" } else { "[set]" });
    print!("Enter API Key (or press Enter to skip): ");
    io::stdout().flush().unwrap();
    let mut api_key = String::new();
    io::stdin().read_line(&mut api_key).unwrap();
    let api_key = if api_key.trim().is_empty() {
        config.api_key
    } else {
        api_key.trim().to_string()
    };

    // Language
    println!("\nCurrent Language: {}", config.language);
    println!("Available: Vietnamese, English, Korean");
    print!("Enter language (or press Enter to keep): ");
    io::stdout().flush().unwrap();
    let mut language = String::new();
    io::stdin().read_line(&mut language).unwrap();
    let language = if language.trim().is_empty() {
        config.language
    } else {
        language.trim().to_string()
    };

    // Hotkey
    println!("\nCurrent Hotkey: {} (VK code)", config.hotkey);
    print!("Enter hotkey code (or press Enter to keep): ");
    io::stdout().flush().unwrap();
    let mut hotkey_str = String::new();
    io::stdin().read_line(&mut hotkey_str).unwrap();
    let hotkey = if hotkey_str.trim().is_empty() {
        config.hotkey
    } else {
        hotkey_str.trim().parse().unwrap_or(192)
    };

    let new_config = Config {
        api_key,
        language,
        hotkey,
    };

    save_config(&new_config);
    println!("\n✓ Config saved!");
}

lazy_static! {
    static ref APP: Arc<Mutex<App>> = Arc::new(Mutex::new(App::new()));
}

impl App {
    fn new() -> Self {
        let runtime = Runtime::new().unwrap();
        let client = Client::new();
        let config = load_config();
        Self {
            config,
            runtime,
            client,
        }
    }

    fn capture_screenshot(&self) -> Result<String> {
        unsafe {
            let screen_dc = GetDC(None);
            let width = GetSystemMetrics(SM_CXSCREEN);
            let height = GetSystemMetrics(SM_CYSCREEN);
            let mem_dc = CreateCompatibleDC(screen_dc);
            let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
            let old_bitmap = SelectObject(mem_dc, bitmap);
            BitBlt(mem_dc, 0, 0, width, height, screen_dc, 0, 0, SRCCOPY)?;

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [RGBQUAD::default()],
            };

            let mut buffer: Vec<u8> = vec![0; (width * height * 4) as usize];
            GetDIBits(mem_dc, bitmap, 0, height as u32, Some(buffer.as_mut_ptr() as *mut std::ffi::c_void), &mut bmi, DIB_RGB_COLORS);

            // BGRA to RGBA
            for pixel in buffer.chunks_exact_mut(4) {
                let b = pixel[0];
                pixel[0] = pixel[2];
                pixel[2] = b;
            }

            let img = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(width as u32, height as u32, buffer).unwrap();
            let mut png_data = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut png_data), ImageFormat::Png)?;
            let base64 = general_purpose::STANDARD.encode(&png_data);

            SelectObject(mem_dc, old_bitmap);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);

            Ok(base64)
        }
    }

    fn get_cursor_pos(&self) -> (i32, i32) {
        unsafe {
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            (point.x, point.y)
        }
    }

    async fn call_groq(&self, base64_img: String, cursor_x: i32, cursor_y: i32) -> Result<GroqResponse> {
        let prompt = format!("Based on the cursor position at ({}, {}), guess the intended text region on the screen that the user wants translated. Extract the text from that region and translate it to {}. Return the translation and the bounding box coordinates of the text region in JSON format with keys 'translation' and 'box' where 'box' has 'x', 'y', 'width', 'height'.", cursor_x, cursor_y, self.config.language);

        let request = serde_json::json!({
            "model": "meta-llama/llama-4-scout-17b-16e-instruct",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": &prompt},
                    {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{}", base64_img)}}
                ]
            }],
            "response_format": {"type": "json_object"},
            "temperature": 0.1,
            "max_completion_tokens": 1024
        });

        let response = self.client
            .post("https://api.groq.com/openai/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        let text = response.text().await?;
        
        if !status.is_success() {
            return Err(anyhow::anyhow!("API Error ({}): {}", status, text));
        }

        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("Failed to parse response: {}. Response was: {}", e, text))?;
        
        let content = json["choices"][0]["message"]["content"].as_str()
            .ok_or_else(|| anyhow::anyhow!("No content in response. Full response: {}", json))?;
        let resp: GroqResponse = serde_json::from_str(content)?;
        Ok(resp)
    }

    fn run(app: Arc<Mutex<App>>) -> Result<()> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class_name = s!("ScreenTranslatorClass");

            let wc = WNDCLASSA {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance.into(),
                hIcon: HICON::default(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hbrBackground: HBRUSH::default(),
                lpszMenuName: PCSTR::null(),
                lpszClassName: class_name,
            };

            RegisterClassA(&wc);

            let hwnd = CreateWindowExA(
                WINDOW_EX_STYLE::default(),
                class_name,
                s!("Screen Translator"),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                400,
                300,
                None,
                None,
                instance,
                None,
            )?;

            let _ = ShowWindow(hwnd, SW_SHOW);

            // Register hotkey
            let hotkey = app.lock().unwrap().config.hotkey;
            println!("Registering hotkey: {}", hotkey);
            match RegisterHotKey(hwnd, 1, HOT_KEY_MODIFIERS(0), hotkey) {
                Ok(_) => println!("Hotkey registered successfully"),
                Err(e) => {
                    eprintln!("Failed to register hotkey: {:?}", e);
                    return Err(e.into());
                }
            }

            let mut msg = MSG::default();
            while GetMessageA(&mut msg, None, 0, 0).into() {
                let _ = TranslateMessage(&msg);
                DispatchMessageA(&msg);
            }

            Ok(())
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            if wparam.0 == 1 {
                let app_clone = Arc::clone(&APP);
                std::thread::spawn(move || {
                    let rt = Runtime::new().unwrap();
                    rt.block_on(async {
                        match app_clone.lock().unwrap().capture_screenshot() {
                            Ok(screenshot) => {
                                let (x, y) = app_clone.lock().unwrap().get_cursor_pos();
                                match app_clone.lock().unwrap().call_groq(screenshot, x, y).await {
                                    Ok(resp) => {
                                        let text = HSTRING::from(&format!("Translation: {}", resp.translation));
                                        unsafe { MessageBoxW(None, &text, w!("Result"), MB_OK); }
                                    }
                                    Err(e) => {
                                        let err_text = HSTRING::from(&format!("Error: {}", e));
                                        unsafe { MessageBoxW(None, &err_text, w!("Error"), MB_OK); }
                                    }
                                }
                            }
                            Err(e) => {
                                let err_text = HSTRING::from(&format!("Screenshot failed: {}", e));
                                unsafe { MessageBoxW(None, &err_text, w!("Error"), MB_OK); }
                            }
                        }
                    });
                });
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcA(hwnd, msg, wparam, lparam),
    }
}

fn main() {
    let config = load_config();
    if config.api_key.is_empty() {
        run_config_prompt();
    } else {
        App::run(APP.clone()).expect("Failed to run app");
    }
}
