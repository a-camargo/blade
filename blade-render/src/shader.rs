use std::{any, collections::HashMap, fmt, fs, path::Path, str, sync::Arc};

const FAILURE_DUMP_NAME: &str = "_failure.wgsl";

#[derive(blade_macros::Flat)]
pub struct CookedShader<'a> {
    data: &'a [u8],
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Meta;
impl fmt::Display for Meta {
    fn fmt(&self, _f: &mut fmt::Formatter) -> fmt::Result {
        Ok(())
    }
}

pub struct Shader {
    pub raw: Result<blade_graphics::Shader, &'static str>,
}

pub enum Expansion {
    Values(HashMap<String, u32>),
    Bool(bool),
}
impl Expansion {
    pub fn from_enum<E: strum::IntoEnumIterator + fmt::Debug + Into<u32>>() -> Self {
        Self::Values(
            E::iter()
                .map(|variant| (format!("{variant:?}"), variant.into()))
                .collect(),
        )
    }
    pub fn from_bitflags<F: bitflags::Flags<Bits = u32>>() -> Self {
        Self::Values(
            F::FLAGS
                .iter()
                .map(|flag| (flag.name().to_string(), flag.value().bits()))
                .collect(),
        )
    }
}

pub struct Baker {
    gpu_context: Arc<blade_graphics::Context>,
    expansions: HashMap<String, Expansion>,
}

impl Baker {
    pub fn new(gpu_context: &Arc<blade_graphics::Context>) -> Self {
        Self {
            gpu_context: Arc::clone(gpu_context),
            expansions: HashMap::default(),
        }
    }

    fn register<T>(&mut self, expansion: Expansion) {
        let full_name = any::type_name::<T>();
        let short_name = full_name.split("::").last().unwrap().to_string();
        self.expansions.insert(short_name, expansion);
    }

    pub fn register_enum<E: strum::IntoEnumIterator + fmt::Debug + Into<u32>>(&mut self) {
        self.register::<E>(Expansion::from_enum::<E>());
    }

    pub fn register_bitflags<F: bitflags::Flags<Bits = u32>>(&mut self) {
        self.register::<F>(Expansion::from_bitflags::<F>());
    }

    pub fn register_bool(&mut self, name: &str, value: bool) {
        self.expansions
            .insert(name.to_string(), Expansion::Bool(value));
    }
}

fn parse_impl(
    text_raw: &[u8],
    base_path: &Path,
    text_out: &mut String,
    cooker: &blade_asset::Cooker<Baker>,
    expansions: &HashMap<String, Expansion>,
) {
    use std::fmt::Write as _;

    let text_in = str::from_utf8(text_raw).unwrap();
    for line in text_in.lines() {
        if line.starts_with("#include") {
            let include_path = match line.split('"').nth(1) {
                Some(include) => base_path.join(include),
                None => panic!("Unable to extract the include path from: {line}"),
            };
            let include = cooker.add_dependency(&include_path);
            writeln!(text_out, "//{}", line).unwrap();
            parse_impl(
                &include,
                include_path.parent().unwrap(),
                text_out,
                cooker,
                expansions,
            );
        } else if line.starts_with("#use") {
            let type_name = line.split_whitespace().last().unwrap();
            match expansions[type_name] {
                Expansion::Values(ref map) => {
                    for (key, value) in map.iter() {
                        writeln!(text_out, "const {}_{}: u32 = {}u;", type_name, key, value)
                            .unwrap();
                    }
                }
                Expansion::Bool(value) => {
                    writeln!(text_out, "const {}: bool = {};", type_name, value).unwrap();
                }
            }
        } else {
            *text_out += line;
        }
        *text_out += "\n";
    }
}

pub fn parse_shader(
    text_raw: &[u8],
    cooker: &blade_asset::Cooker<Baker>,
    expansions: &HashMap<String, Expansion>,
) -> String {
    let mut text_out = String::new();
    parse_impl(text_raw, ".".as_ref(), &mut text_out, cooker, expansions);
    text_out
}

impl blade_asset::Baker for Baker {
    type Meta = Meta;
    type Data<'a> = CookedShader<'a>;
    type Output = Shader;
    fn cook(
        &self,
        source: &[u8],
        extension: &str,
        _meta: Meta,
        cooker: Arc<blade_asset::Cooker<Self>>,
        _exe_context: &choir::ExecutionContext,
    ) {
        assert_eq!(extension, "wgsl");
        let text_out = parse_shader(source, &cooker, &self.expansions);
        cooker.finish(CookedShader {
            data: text_out.as_bytes(),
        });
    }
    fn serve(&self, cooked: CookedShader, _exe_context: &choir::ExecutionContext) -> Shader {
        let source = str::from_utf8(cooked.data).unwrap();
        let raw = self
            .gpu_context
            .try_create_shader(blade_graphics::ShaderDesc { source });
        if let Err(e) = raw {
            let _ = fs::write(FAILURE_DUMP_NAME, source);
            log::warn!("Shader compilation failed: {e:?}, source dumped as '{FAILURE_DUMP_NAME}'.")
        }
        Shader { raw }
    }
    fn delete(&self, _output: Shader) {}
}
