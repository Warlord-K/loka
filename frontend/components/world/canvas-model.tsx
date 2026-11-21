"use client";

import { useEffect, useRef, useState } from "react";
import { Shimmer } from "./work-ui";

export default function CanvasModel({ src, label }: { src: string; label: string }) {
  const host = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<"loading" | "ready" | "failed">("loading");

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    let stopped = false;
    let cleanup = () => {};
    (async () => {
      const THREE = await import("three");
      const { GLTFLoader } = await import("three/examples/jsm/loaders/GLTFLoader.js");
      const { OrbitControls } = await import("three/examples/jsm/controls/OrbitControls.js");
      if (stopped) return;
      const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
      renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
      renderer.setSize(el.clientWidth, el.clientHeight);
      renderer.domElement.className = "nodrag nowheel h-full w-full";
      el.appendChild(renderer.domElement);
      const scene = new THREE.Scene();
      scene.add(new THREE.HemisphereLight("white", "dimgray", 2.4));
      const sun = new THREE.DirectionalLight("white", 1.6);
      sun.position.set(3, 5, 4);
      scene.add(sun);
      const camera = new THREE.PerspectiveCamera(40, el.clientWidth / Math.max(1, el.clientHeight), 0.01, 1000);
      const controls = new OrbitControls(camera, renderer.domElement);
      controls.enableDamping = !still;
      controls.autoRotate = !still;
      controls.autoRotateSpeed = 1.2;
      let frame = 0;
      const loop = () => {
        controls.update();
        renderer.render(scene, camera);
        frame = requestAnimationFrame(loop);
      };
      const resize = new ResizeObserver(() => {
        renderer.setSize(el.clientWidth, el.clientHeight);
        camera.aspect = el.clientWidth / Math.max(1, el.clientHeight);
        camera.updateProjectionMatrix();
      });
      resize.observe(el);
      cleanup = () => {
        cancelAnimationFrame(frame);
        resize.disconnect();
        controls.dispose();
        renderer.dispose();
        renderer.domElement.remove();
      };
      new GLTFLoader().load(
        src,
        (gltf) => {
          if (stopped) return;
          const box = new THREE.Box3().setFromObject(gltf.scene);
          const size = box.getSize(new THREE.Vector3()).length() || 1;
          const center = box.getCenter(new THREE.Vector3());
          gltf.scene.position.sub(center);
          scene.add(gltf.scene);
          camera.position.set(size * 0.7, size * 0.5, size * 0.9);
          camera.near = size / 100;
          camera.far = size * 100;
          camera.updateProjectionMatrix();
          controls.target.set(0, 0, 0);
          setState("ready");
          loop();
        },
        undefined,
        () => !stopped && setState("failed"),
      );
    })().catch(() => !stopped && setState("failed"));
    return () => {
      stopped = true;
      cleanup();
    };
  }, [src]);

  return (
    <div ref={host} role="img" aria-label={label} className="relative h-full w-full">
      {state !== "ready" && (
        <div className="absolute inset-0 grid place-items-center text-w-xs text-w-muted">
          {state === "loading" ? <Shimmer>Loading the model</Shimmer> : "This model could not be opened."}
        </div>
      )}
    </div>
  );
}
