//! Code-word semantic model, inference only (`bench/train_semantic.py` fits it).
//!
//! Two embedding tables: view A (words that *describe* a function: name, enclosing class, signature,
//! docstring) and view B (identifiers that *implement* it). A text is embedded as the L2-normalised,
//! idf-weighted, sublinear-tf-weighted sum of its word vectors. Training lives in the Python toolkit.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use half::f16;

use crate::npy::load_npz_bytes;

pub struct SemanticModel {
    pub va: HashMap<String, usize>,
    pub vb: HashMap<String, usize>,
    idf_a: Vec<f32>,
    idf_b: Vec<f32>,
    u16: Vec<f16>, // [|VA|, k] as stored
    v16: Vec<f16>, // [|VB|, k]
    w: Vec<f32>,   // (s / s[0]) ^ gamma, multiplies every dimension
    pub k: usize,
    pub wa: f32,
    pub cfg: serde_json::Value,
    pub n_pairs: Option<i64>,
    u32: OnceLock<Vec<f32>>,
    v32: OnceLock<Vec<f32>>,
}

fn sublog(c: u32) -> f64 {
    1.0 + (c as f64).ln()
}

impl SemanticModel {
    pub fn load(path: &Path) -> Result<SemanticModel> {
        let bytes = std::fs::read(path).with_context(|| format!("load semantic model {}", path.display()))?;
        Self::from_arrays(load_npz_bytes(&bytes).with_context(|| format!("load semantic model {}", path.display()))?)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<SemanticModel> {
        Self::from_arrays(load_npz_bytes(bytes).context("load embedded semantic model")?)
    }

    fn from_arrays(arrays: Vec<(String, crate::npy::Npy)>) -> Result<SemanticModel> {
        let mut map: HashMap<String, crate::npy::Npy> = arrays.into_iter().collect();
        let mut take = |name: &str| map.remove(name).ok_or_else(|| anyhow!("model lacks {name}"));
        let va = take("va")?.to_strings()?;
        let vb = take("vb")?.to_strings()?;
        let idf_a = take("idf_a")?.to_vec::<f32>()?;
        let idf_b = take("idf_b")?.to_vec::<f32>()?;
        let u = take("U")?;
        let k = u.shape[1];
        let u16 = u.to_vec::<f16>()?;
        let v16 = take("V")?.to_vec::<f16>()?;
        let s = take("s")?.to_vec::<f32>()?;
        let cfg_s = take("cfg")?.to_strings()?.into_iter().next().unwrap_or_else(|| "{}".into());
        let cfg: serde_json::Value = serde_json::from_str(&cfg_s).unwrap_or(serde_json::Value::Null);
        let gamma = cfg.get("gamma").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
        let wa = cfg.get("wa").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
        let n_pairs = cfg.get("n_pairs").and_then(|v| v.as_i64());
        let w: Vec<f32> = s.iter().map(|&x| (x / s[0]).powf(gamma)).collect();
        Ok(SemanticModel {
            va: va.into_iter().enumerate().map(|(i, t)| (t, i)).collect(),
            vb: vb.into_iter().enumerate().map(|(i, t)| (t, i)).collect(),
            idf_a,
            idf_b,
            u16,
            v16,
            w,
            k,
            wa,
            cfg,
            n_pairs,
            u32: OnceLock::new(),
            v32: OnceLock::new(),
        })
    }

    /// Build the weighted float32 tables (worth it before embedding many chunks).
    pub fn warm(&self) {
        self.u32.get_or_init(|| self.expand(&self.u16));
        self.v32.get_or_init(|| self.expand(&self.v16));
    }

    fn expand(&self, t: &[f16]) -> Vec<f32> {
        let k = self.k;
        t.iter().enumerate().map(|(i, v)| v.to_f32() * self.w[i % k]).collect()
    }

    fn row_into(&self, table: &[f16], warmed: &OnceLock<Vec<f32>>, i: usize, out: &mut [f32]) {
        let k = self.k;
        if let Some(full) = warmed.get() {
            out.copy_from_slice(&full[i * k..(i + 1) * k]);
        } else {
            for j in 0..k {
                out[j] = table[i * k + j].to_f32() * self.w[j];
            }
        }
    }

    /// Weighted sum of word vectors, accumulated in float64, as the trainer pools them
    /// (`python/scrutate_ref/semantic.py`).
    fn proj(&self, counts: &[(usize, u32)], idf: &[f32], table: &[f16], warmed: &OnceLock<Vec<f32>>) -> Option<Vec<f64>> {
        if counts.is_empty() {
            return None;
        }
        let ws: Vec<f64> = counts.iter().map(|&(i, c)| sublog(c) * idf[i] as f64).collect();
        let n = ws.iter().map(|w| w * w).sum::<f64>().sqrt();
        if n == 0.0 {
            return None;
        }
        let mut z = vec![0f64; self.k];
        let mut row = vec![0f32; self.k];
        for (&(i, _), w) in counts.iter().zip(&ws) {
            let wn = w / n;
            self.row_into(table, warmed, i, &mut row);
            for j in 0..self.k {
                z[j] += wn * row[j] as f64;
            }
        }
        Some(z)
    }

    fn lookup(counts: &HashMap<&str, u32>, vocab: &HashMap<String, usize>) -> Vec<(usize, u32)> {
        let mut out: Vec<(usize, u32)> = counts.iter().filter_map(|(t, &c)| vocab.get(*t).map(|&i| (i, c))).collect();
        out.sort_unstable();
        out
    }

    fn normalize(z: Vec<f64>) -> Option<Vec<f32>> {
        let n = z.iter().map(|v| v * v).sum::<f64>().sqrt();
        if n > 0.0 {
            Some(z.iter().map(|v| (v / n) as f32).collect())
        } else {
            None
        }
    }

    /// Unit vector of a chunk from its token lists (any list may be empty).
    pub fn encode_chunk(&self, name: &[String], qual: &[String], sig: &[String], doc: &[String], body: &[String]) -> Option<Vec<f32>> {
        let mut a: HashMap<&str, u32> = HashMap::new();
        for t in name {
            *a.entry(t.as_str()).or_insert(0) += 3;
        }
        for t in qual.iter().chain(sig).chain(doc) {
            *a.entry(t.as_str()).or_insert(0) += 1;
        }
        let mut b: HashMap<&str, u32> = HashMap::new();
        for t in body {
            *b.entry(t.as_str()).or_insert(0) += 1;
        }
        let za = self.proj(&Self::lookup(&a, &self.va), &self.idf_a, &self.u16, &self.u32);
        let zb = self.proj(&Self::lookup(&b, &self.vb), &self.idf_b, &self.v16, &self.v32);
        if za.is_none() && zb.is_none() {
            return None;
        }
        let mut z = vec![0f64; self.k];
        let wa = self.wa as f64;
        if let Some(za) = za {
            for j in 0..self.k {
                z[j] += wa * za[j];
            }
        }
        if let Some(zb) = zb {
            for j in 0..self.k {
                z[j] += (1.0 - wa) * zb[j];
            }
        }
        Self::normalize(z)
    }

    /// Unit vector of a query from its (unique) stemmed terms.
    pub fn encode_query(&self, terms: &[String]) -> Option<Vec<f32>> {
        let mut a: HashMap<&str, u32> = HashMap::new();
        for t in terms {
            *a.entry(t.as_str()).or_insert(0) += 1;
        }
        let z = self.proj(&Self::lookup(&a, &self.va), &self.idf_a, &self.u16, &self.u32)?;
        Self::normalize(z)
    }
}
